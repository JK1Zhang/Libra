//! This module startups all the components of a TiKV server.
//!
//! It is responsible for reading from configs, starting up the various server components,
//! and handling errors (mostly by aborting and reporting to the user).
//!
//! The entry point is `run_tikv`.
//!
//! Components are often used to initialize other components, and/or must be explicitly stopped.
//! We keep these components in the `TiKVServer` struct.

use std::{
    convert::TryFrom,
    env, fmt,
    fs::{self, File},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

use concurrency_manager::ConcurrencyManager;
use encryption::DataKeyManager;
use engine_rocks::{encryption::get_env, RocksEngine};
use engine_traits::{
    compaction_job::CompactionJobInfo, Engines, MetricsFlusher, RaftEngine, CF_DEFAULT, CF_WRITE,
};
use fs2::FileExt;
use futures::executor::block_on;
use kvproto::{
    backup::create_backup, cdcpb::create_change_data, deadlock::create_deadlock,
    debugpb::create_debug, diagnosticspb::create_diagnostics, import_sstpb::create_import_sst,
};
use pd_client::{PdClient, RpcClient};
use raft_log_engine::RaftLogEngine;
use raftstore::{
    coprocessor::{
        config::SplitCheckConfigManager, BoxConsistencyCheckObserver, ConsistencyCheckMethod,
        CoprocessorHost, RawConsistencyCheckObserver, RegionInfoAccessor,
    },
    router::ServerRaftStoreRouter,
    store::{
        config::RaftstoreConfigManager,
        fsm,
        fsm::store::{RaftBatchSystem, RaftRouter, StoreMeta, PENDING_VOTES_CAP},
        AutoSplitController, GlobalReplicationState, LocalReader, SnapManagerBuilder,
        SplitCheckRunner, SplitConfigManager, StoreMsg,
    },
};
use security::SecurityManager;
use tikv::{
    config::{ConfigController, DBConfigManger, DBType, TiKvConfig},
    coprocessor,
    import::{ImportSSTService, SSTImporter},
    read_pool::{build_yatp_read_pool, ReadPool},
    server::{
        config::Config as ServerConfig,
        create_raft_storage,
        gc_worker::{AutoGcConfig, GcWorker},
        lock_manager::LockManager,
        resolve,
        service::{DebugService, DiagnosticsService},
        status_server::StatusServer,
        Node, RaftKv, Server, CPU_CORES_QUOTA_GAUGE, DEFAULT_CLUSTER_ID,
    },
    storage::{self, config::StorageConfigManger},
};
use tikv_util::config::VersionTrack;
use tikv_util::{
    check_environment_variables,
    config::ensure_dir_exist,
    sys::sys_quota::SysQuota,
    time::Monitor,
    worker::{FutureWorker, Worker},
};
use tokio::runtime::Builder;

use crate::{setup::*, signal_handler};

/// Run a TiKV server. Returns when the server is shutdown by the user, in which
/// case the server will be properly stopped.
pub fn run_tikv(config: TiKvConfig) {
    // Sets the global logger ASAP.
    // It is okay to use the config w/o `validate()`,
    // because `initial_logger()` handles various conditions.
    initial_logger(&config);

    // Print version information.
    tikv::log_tikv_info();

    // Print resource quota.
    SysQuota::new().log_quota();
    CPU_CORES_QUOTA_GAUGE.set(SysQuota::new().cpu_cores_quota());

    // Do some prepare works before start.
    pre_start();

    let _m = Monitor::default();

    macro_rules! run_impl {
        ($ER: ty) => {{
            let mut tikv = TiKVServer::<$ER>::init(config);
            tikv.check_conflict_addr();
            tikv.init_fs();
            tikv.init_yatp();
            tikv.init_encryption();
            let engines = tikv.init_raw_engines();
            tikv.init_engines(engines);
            let gc_worker = tikv.init_gc_worker();
            let server_config = tikv.init_servers(&gc_worker);
            tikv.register_services();
            tikv.init_metrics_flusher();
            tikv.run_server(server_config);
            tikv.run_status_server();

            signal_handler::wait_for_signal(Some(tikv.engines.take().unwrap().engines));
            tikv.stop();
        }};
    }

    if !config.raft_engine.enable {
        run_impl!(RocksEngine)
    } else {
        run_impl!(RaftLogEngine)
    }
}

const RESERVED_OPEN_FDS: u64 = 1000;

/// A complete TiKV server.
struct TiKVServer<ER: RaftEngine> {
    config: TiKvConfig,
    cfg_controller: Option<ConfigController>,
    security_mgr: Arc<SecurityManager>,
    pd_client: Arc<RpcClient>,
    router: RaftRouter<RocksEngine, ER>,
    system: Option<RaftBatchSystem<RocksEngine, ER>>,
    resolver: resolve::PdStoreAddrResolver,
    state: Arc<Mutex<GlobalReplicationState>>,
    store_path: PathBuf,
    encryption_key_manager: Option<Arc<DataKeyManager>>,
    engines: Option<TiKVEngines<ER>>,
    servers: Option<Servers<ER>>,
    region_info_accessor: RegionInfoAccessor,
    coprocessor_host: Option<CoprocessorHost<RocksEngine>>,
    to_stop: Vec<Box<dyn Stop>>,
    lock_files: Vec<File>,
    concurrency_manager: ConcurrencyManager,
}

struct TiKVEngines<ER: RaftEngine> {
    engines: Engines<RocksEngine, ER>,
    store_meta: Arc<Mutex<StoreMeta>>,
    engine: RaftKv<ServerRaftStoreRouter<RocksEngine, ER>>,
}

struct Servers<ER: RaftEngine> {
    lock_mgr: LockManager,
    server: Server<RaftRouter<RocksEngine, ER>, resolve::PdStoreAddrResolver>,
    node: Node<RpcClient, ER>,
    importer: Arc<SSTImporter>,
    cdc_scheduler: tikv_util::worker::Scheduler<cdc::Task>,
}

impl<ER: RaftEngine> TiKVServer<ER> {
    fn init(mut config: TiKvConfig) -> TiKVServer<ER> {
        // It is okay use pd config and security config before `init_config`,
        // because these configs must be provided by command line, and only
        // used during startup process.
        let security_mgr = Arc::new(
            SecurityManager::new(&config.security)
                .unwrap_or_else(|e| fatal!("failed to create security manager: {}", e)),
        );
        let pd_client = Self::connect_to_pd_cluster(&mut config, Arc::clone(&security_mgr));

        // Initialize and check config
        let cfg_controller = Self::init_config(config);
        let config = cfg_controller.get_current();

        let store_path = Path::new(&config.storage.data_dir).to_owned();

        // Initialize raftstore channels.
        let (router, system) = fsm::create_raft_batch_system(&config.raft_store);

        let (resolve_worker, resolver, state) =
            resolve::new_resolver(Arc::clone(&pd_client), router.clone())
                .unwrap_or_else(|e| fatal!("failed to start address resolver: {}", e));

        let mut coprocessor_host = Some(CoprocessorHost::new(router.clone()));
        match config.coprocessor.consistency_check_method {
            ConsistencyCheckMethod::Mvcc => {
                // TODO: use mvcc consistency checker.
                coprocessor_host
                    .as_mut()
                    .unwrap()
                    .registry
                    .register_consistency_check_observer(
                        100,
                        BoxConsistencyCheckObserver::new(RawConsistencyCheckObserver::default()),
                    );
            }
            ConsistencyCheckMethod::Raw => {
                coprocessor_host
                    .as_mut()
                    .unwrap()
                    .registry
                    .register_consistency_check_observer(
                        100,
                        BoxConsistencyCheckObserver::new(RawConsistencyCheckObserver::default()),
                    );
            }
        }
        let region_info_accessor = RegionInfoAccessor::new(coprocessor_host.as_mut().unwrap());
        region_info_accessor.start();

        // Initialize concurrency manager
        let latest_ts = block_on(pd_client.get_tso()).expect("failed to get timestamp from PD");
        let concurrency_manager = ConcurrencyManager::new(latest_ts.into());

        TiKVServer {
            config,
            cfg_controller: Some(cfg_controller),
            security_mgr,
            pd_client,
            router,
            system: Some(system),
            resolver,
            state,
            store_path,
            encryption_key_manager: None,
            engines: None,
            servers: None,
            region_info_accessor,
            coprocessor_host,
            to_stop: vec![Box::new(resolve_worker)],
            lock_files: vec![],
            concurrency_manager,
        }
    }

    /// Initialize and check the config
    ///
    /// Warnings are logged and fatal errors exist.
    ///
    /// #  Fatal errors
    ///
    /// - If `dynamic config` feature is enabled and failed to register config to PD
    /// - If some critical configs (like data dir) are differrent from last run
    /// - If the config can't pass `validate()`
    /// - If the max open file descriptor limit is not high enough to support
    ///   the main database and the raft database.
    fn init_config(mut config: TiKvConfig) -> ConfigController {
        ensure_dir_exist(&config.storage.data_dir).unwrap();
        ensure_dir_exist(&config.raft_store.raftdb_path).unwrap();

        validate_and_persist_config(&mut config, true);
        check_system_config(&config);

        tikv_util::set_panic_hook(false, &config.storage.data_dir);

        info!(
            "using config";
            "config" => serde_json::to_string(&config).unwrap(),
        );
        if config.panic_when_unexpected_key_or_data {
            info!("panic-when-unexpected-key-or-data is on");
            tikv_util::set_panic_when_unexpected_key_or_data(true);
        }

        config.write_into_metrics();

        ConfigController::new(config)
    }

    fn connect_to_pd_cluster(
        config: &mut TiKvConfig,
        security_mgr: Arc<SecurityManager>,
    ) -> Arc<RpcClient> {
        let pd_client = Arc::new(
            RpcClient::new(&config.pd, security_mgr)
                .unwrap_or_else(|e| fatal!("failed to create rpc client: {}", e)),
        );

        let cluster_id = pd_client
            .get_cluster_id()
            .unwrap_or_else(|e| fatal!("failed to get cluster id: {}", e));
        if cluster_id == DEFAULT_CLUSTER_ID {
            fatal!("cluster id can't be {}", DEFAULT_CLUSTER_ID);
        }
        config.server.cluster_id = cluster_id;
        info!(
            "connect to PD cluster";
            "cluster_id" => cluster_id
        );

        pd_client
    }

    fn check_conflict_addr(&mut self) {
        let cur_addr: SocketAddr = self
            .config
            .server
            .addr
            .parse()
            .expect("failed to parse into a socket address");
        let cur_ip = cur_addr.ip();
        let cur_port = cur_addr.port();
        let lock_dir = get_lock_dir();

        let search_base = env::temp_dir().join(&lock_dir);
        std::fs::create_dir_all(&search_base)
            .unwrap_or_else(|_| panic!("create {} failed", search_base.display()));

        for result in fs::read_dir(&search_base).unwrap() {
            if let Ok(entry) = result {
                if !entry.file_type().unwrap().is_file() {
                    continue;
                }
                let file_path = entry.path();
                let file_name = file_path.file_name().unwrap().to_str().unwrap();
                if let Ok(addr) = file_name.replace('_', ":").parse::<SocketAddr>() {
                    let ip = addr.ip();
                    let port = addr.port();
                    if cur_port == port
                        && (cur_ip == ip || cur_ip.is_unspecified() || ip.is_unspecified())
                    {
                        let _ = try_lock_conflict_addr(file_path);
                    }
                }
            }
        }

        let cur_path = search_base.join(cur_addr.to_string().replace(':', "_"));
        let cur_file = try_lock_conflict_addr(cur_path);
        self.lock_files.push(cur_file);
    }

    fn init_fs(&mut self) {
        let lock_path = self.store_path.join(Path::new("LOCK"));

        let f = File::create(lock_path.as_path())
            .unwrap_or_else(|e| fatal!("failed to create lock at {}: {}", lock_path.display(), e));
        if f.try_lock_exclusive().is_err() {
            fatal!(
                "lock {} failed, maybe another instance is using this directory.",
                self.store_path.display()
            );
        }
        self.lock_files.push(f);

        if tikv_util::panic_mark_file_exists(&self.config.storage.data_dir) {
            fatal!(
                "panic_mark_file {} exists, there must be something wrong with the db.",
                tikv_util::panic_mark_file_path(&self.config.storage.data_dir).display()
            );
        }

        // We truncate a big file to make sure that both raftdb and kvdb of TiKV have enough space
        // to compaction when TiKV recover. This file is created in data_dir rather than db_path,
        // because we must not increase store size of db_path.
        tikv_util::reserve_space_for_recover(
            &self.config.storage.data_dir,
            self.config.storage.reserve_space.0,
        )
        .unwrap();
    }

    fn init_yatp(&self) {
        yatp::metrics::set_namespace(Some("tikv"));
        prometheus::register(Box::new(yatp::metrics::MULTILEVEL_LEVEL0_CHANCE.clone())).unwrap();
        prometheus::register(Box::new(yatp::metrics::MULTILEVEL_LEVEL_ELAPSED.clone())).unwrap();
    }

    fn init_encryption(&mut self) {
        self.encryption_key_manager = DataKeyManager::from_config(
            &self.config.security.encryption,
            &self.config.storage.data_dir,
        )
        .unwrap()
        .map(|key_manager| Arc::new(key_manager));
    }

    fn create_raftstore_compaction_listener(&self) -> engine_rocks::CompactionListener {
        fn size_change_filter(info: &engine_rocks::RocksCompactionJobInfo) -> bool {
            // When calculating region size, we only consider write and default
            // column families.
            let cf = info.cf_name();
            if cf != CF_WRITE && cf != CF_DEFAULT {
                return false;
            }
            // Compactions in level 0 and level 1 are very frequently.
            if info.output_level() < 2 {
                return false;
            }

            true
        }

        let ch = Mutex::new(self.router.clone());
        let compacted_handler = Box::new(move |compacted_event: engine_rocks::CompactedEvent| {
            let ch = ch.lock().unwrap();
            let event = StoreMsg::CompactedEvent(compacted_event);
            if let Err(e) = ch.send_control(event) {
                error!(?e; "send compaction finished event to raftstore failed");
            }
        });
        engine_rocks::CompactionListener::new(compacted_handler, Some(size_change_filter))
    }

    fn init_engines(&mut self, engines: Engines<RocksEngine, ER>) {
        let store_meta = Arc::new(Mutex::new(StoreMeta::new(PENDING_VOTES_CAP)));
        let engine = RaftKv::new(
            ServerRaftStoreRouter::new(
                self.router.clone(),
                LocalReader::new(engines.kv.clone(), store_meta.clone(), self.router.clone()),
            ),
            engines.kv.clone(),
        );

        let cfg_controller = self.cfg_controller.as_mut().unwrap();
        cfg_controller.register(
            tikv::config::Module::Storage,
            Box::new(StorageConfigManger::new(
                engines.kv.clone(),
                self.config.storage.block_cache.shared,
            )),
        );

        self.engines = Some(TiKVEngines {
            engines,
            store_meta,
            engine,
        });
    }

    fn init_gc_worker(
        &mut self,
    ) -> GcWorker<RaftKv<ServerRaftStoreRouter<RocksEngine, ER>>, RaftRouter<RocksEngine, ER>> {
        let engines = self.engines.as_ref().unwrap();
        let mut gc_worker = GcWorker::new(
            engines.engine.clone(),
            self.router.clone(),
            self.config.gc.clone(),
            self.pd_client.cluster_version(),
        );
        gc_worker
            .start()
            .unwrap_or_else(|e| fatal!("failed to start gc worker: {}", e));
        gc_worker
            .start_observe_lock_apply(self.coprocessor_host.as_mut().unwrap())
            .unwrap_or_else(|e| fatal!("gc worker failed to observe lock apply: {}", e));

        gc_worker
    }

    fn init_servers(
        &mut self,
        gc_worker: &GcWorker<
            RaftKv<ServerRaftStoreRouter<RocksEngine, ER>>,
            RaftRouter<RocksEngine, ER>,
        >,
    ) -> Arc<ServerConfig> {
        let cfg_controller = self.cfg_controller.as_mut().unwrap();
        cfg_controller.register(
            tikv::config::Module::Gc,
            Box::new(gc_worker.get_config_manager()),
        );

        // Create cdc.
        let mut cdc_worker = Box::new(tikv_util::worker::Worker::new("cdc"));
        let cdc_scheduler = cdc_worker.scheduler();
        let txn_extra_scheduler = cdc::CdcTxnExtraScheduler::new(cdc_scheduler.clone());

        self.engines
            .as_mut()
            .unwrap()
            .engine
            .set_txn_extra_scheduler(Arc::new(txn_extra_scheduler));

        // Create CoprocessorHost.
        let mut coprocessor_host = self.coprocessor_host.take().unwrap();

        let lock_mgr = LockManager::new();
        cfg_controller.register(
            tikv::config::Module::PessimisticTxn,
            Box::new(lock_mgr.config_manager()),
        );
        lock_mgr.register_detector_role_change_observer(&mut coprocessor_host);

        let engines = self.engines.as_ref().unwrap();

        let pd_worker = FutureWorker::new("pd-worker");
        let pd_sender = pd_worker.scheduler();

        let unified_read_pool = if self.config.readpool.is_unified_pool_enabled() {
            Some(build_yatp_read_pool(
                &self.config.readpool.unified,
                pd_sender.clone(),
                engines.engine.clone(),
            ))
        } else {
            None
        };

        // The `DebugService` and `DiagnosticsService` will share the same thread pool
        let debug_thread_pool = Arc::new(
            Builder::new()
                .threaded_scheduler()
                .thread_name(thd_name!("debugger"))
                .core_threads(1)
                .on_thread_start(|| tikv_alloc::add_thread_memory_accessor())
                .on_thread_stop(|| tikv_alloc::remove_thread_memory_accessor())
                .build()
                .unwrap(),
        );

        let storage_read_pool_handle = if self.config.readpool.storage.use_unified_pool() {
            unified_read_pool.as_ref().unwrap().handle()
        } else {
            let storage_read_pools = ReadPool::from(storage::build_read_pool(
                &self.config.readpool.storage,
                pd_sender.clone(),
                engines.engine.clone(),
            ));
            storage_read_pools.handle()
        };

        let storage = create_raft_storage(
            engines.engine.clone(),
            &self.config.storage,
            pd_sender.clone(),
            storage_read_pool_handle,
            lock_mgr.clone(),
            self.concurrency_manager.clone(),
            self.config.pessimistic_txn.pipelined,
        )
        .unwrap_or_else(|e| fatal!("failed to create raft storage: {}", e));

        // Create snapshot manager, server.
        let snap_path = self
            .store_path
            .join(Path::new("snap"))
            .to_str()
            .unwrap()
            .to_owned();

        let bps = i64::try_from(self.config.server.snap_max_write_bytes_per_sec.0)
            .unwrap_or_else(|_| fatal!("snap_max_write_bytes_per_sec > i64::max_value"));

        let snap_mgr = SnapManagerBuilder::default()
            .max_write_bytes_per_sec(bps)
            .max_total_size(self.config.server.snap_max_total_size.0)
            .encryption_key_manager(self.encryption_key_manager.clone())
            .build(snap_path);

        // Create coprocessor endpoint.
        let cop_read_pool_handle = if self.config.readpool.coprocessor.use_unified_pool() {
            unified_read_pool.as_ref().unwrap().handle()
        } else {
            let cop_read_pools = ReadPool::from(coprocessor::readpool_impl::build_read_pool(
                &self.config.readpool.coprocessor,
                pd_sender,
                engines.engine.clone(),
            ));
            cop_read_pools.handle()
        };

        // Register cdc
        let cdc_ob = cdc::CdcObserver::new(cdc_scheduler.clone());
        cdc_ob.register_to(&mut coprocessor_host);

        let server_config = Arc::new(self.config.server.clone());

        // Create server
        let server = Server::new(
            &server_config,
            &self.security_mgr,
            storage,
            coprocessor::Endpoint::new(
                &server_config,
                cop_read_pool_handle,
                self.concurrency_manager.clone(),
            ),
            self.router.clone(),
            self.resolver.clone(),
            snap_mgr.clone(),
            gc_worker.clone(),
            unified_read_pool,
            debug_thread_pool,
        )
        .unwrap_or_else(|e| fatal!("failed to create server: {}", e));

        let import_path = self.store_path.join("import");
        let importer =
            Arc::new(SSTImporter::new(import_path, self.encryption_key_manager.clone()).unwrap());

        let mut split_check_worker = Worker::new("split-check");
        let split_check_runner = SplitCheckRunner::new(
            engines.engines.kv.clone(),
            self.router.clone(),
            coprocessor_host.clone(),
            self.config.coprocessor.clone(),
        );
        split_check_worker.start(split_check_runner).unwrap();
        cfg_controller.register(
            tikv::config::Module::Coprocessor,
            Box::new(SplitCheckConfigManager(split_check_worker.scheduler())),
        );

        self.config
            .raft_store
            .validate()
            .unwrap_or_else(|e| fatal!("failed to validate raftstore config {}", e));
        let raft_store = Arc::new(VersionTrack::new(self.config.raft_store.clone()));
        cfg_controller.register(
            tikv::config::Module::Raftstore,
            Box::new(RaftstoreConfigManager(raft_store.clone())),
        );

        let split_config_manager =
            SplitConfigManager(Arc::new(VersionTrack::new(self.config.split.clone())));
        cfg_controller.register(
            tikv::config::Module::Split,
            Box::new(split_config_manager.clone()),
        );

        let auto_split_controller = AutoSplitController::new(split_config_manager);

        let mut node = Node::new(
            self.system.take().unwrap(),
            &server_config,
            raft_store,
            self.pd_client.clone(),
            self.state.clone(),
        );

        node.start(
            engines.engines.clone(),
            server.transport(),
            snap_mgr,
            pd_worker,
            engines.store_meta.clone(),
            coprocessor_host,
            importer.clone(),
            split_check_worker,
            auto_split_controller,
            self.concurrency_manager.clone(),
        )
        .unwrap_or_else(|e| fatal!("failed to start node: {}", e));

        initial_metric(&self.config.metric, Some(node.id()));

        // Start auto gc
        let auto_gc_config = AutoGcConfig::new(
            self.pd_client.clone(),
            self.region_info_accessor.clone(),
            node.id(),
        );
        if let Err(e) = gc_worker.start_auto_gc(auto_gc_config) {
            fatal!("failed to start auto_gc on storage, error: {}", e);
        }

        // Start CDC.
        let cdc_endpoint = cdc::Endpoint::new(
            &self.config.cdc,
            self.pd_client.clone(),
            cdc_worker.scheduler(),
            self.router.clone(),
            cdc_ob,
            engines.store_meta.clone(),
            self.concurrency_manager.clone(),
        );
        let cdc_timer = cdc_endpoint.new_timer();
        cdc_worker
            .start_with_timer(cdc_endpoint, cdc_timer)
            .unwrap_or_else(|e| fatal!("failed to start cdc: {}", e));
        self.to_stop.push(cdc_worker);

        self.servers = Some(Servers {
            lock_mgr,
            server,
            node,
            importer,
            cdc_scheduler,
        });

        server_config
    }

    fn register_services(&mut self) {
        let servers = self.servers.as_mut().unwrap();
        let engines = self.engines.as_ref().unwrap();

        // Import SST service.
        let import_service = ImportSSTService::new(
            self.config.import.clone(),
            self.router.clone(),
            engines.engines.kv.clone(),
            servers.importer.clone(),
            self.security_mgr.clone(),
        );
        if servers
            .server
            .register_service(create_import_sst(import_service))
            .is_some()
        {
            fatal!("failed to register import service");
        }

        // Debug service.
        let debug_service = DebugService::new(
            engines.engines.clone(),
            servers.server.get_debug_thread_pool().clone(),
            self.router.clone(),
            self.cfg_controller.as_ref().unwrap().clone(),
            self.security_mgr.clone(),
        );
        if servers
            .server
            .register_service(create_debug(debug_service))
            .is_some()
        {
            fatal!("failed to register debug service");
        }

        // Create Diagnostics service
        let diag_service = DiagnosticsService::new(
            servers.server.get_debug_thread_pool().clone(),
            self.config.log_file.clone(),
            self.config.slow_log_file.clone(),
            self.security_mgr.clone(),
        );
        if servers
            .server
            .register_service(create_diagnostics(diag_service))
            .is_some()
        {
            fatal!("failed to register diagnostics service");
        }

        // Lock manager.
        if servers
            .server
            .register_service(create_deadlock(
                servers.lock_mgr.deadlock_service(self.security_mgr.clone()),
            ))
            .is_some()
        {
            fatal!("failed to register deadlock service");
        }

        servers
            .lock_mgr
            .start(
                servers.node.id(),
                self.pd_client.clone(),
                self.resolver.clone(),
                self.security_mgr.clone(),
                &self.config.pessimistic_txn,
            )
            .unwrap_or_else(|e| fatal!("failed to start lock manager: {}", e));

        // Backup service.
        let mut backup_worker = Box::new(tikv_util::worker::Worker::new("backup-endpoint"));
        let backup_scheduler = backup_worker.scheduler();
        let backup_service = backup::Service::new(backup_scheduler, self.security_mgr.clone());
        if servers
            .server
            .register_service(create_backup(backup_service))
            .is_some()
        {
            fatal!("failed to register backup service");
        }

        let backup_endpoint = backup::Endpoint::new(
            servers.node.id(),
            engines.engine.clone(),
            self.region_info_accessor.clone(),
            engines.engines.kv.as_inner().clone(),
            self.config.backup.clone(),
            self.concurrency_manager.clone(),
        );
        self.cfg_controller.as_mut().unwrap().register(
            tikv::config::Module::Backup,
            Box::new(backup_endpoint.get_config_manager()),
        );
        let backup_timer = backup_endpoint.new_timer();
        backup_worker
            .start_with_timer(backup_endpoint, backup_timer)
            .unwrap_or_else(|e| fatal!("failed to start backup endpoint: {}", e));

        let cdc_service =
            cdc::Service::new(servers.cdc_scheduler.clone(), self.security_mgr.clone());
        if servers
            .server
            .register_service(create_change_data(cdc_service))
            .is_some()
        {
            fatal!("failed to register cdc service");
        }

        self.to_stop.push(backup_worker);
    }

    fn init_metrics_flusher(&mut self) {
        let mut metrics_flusher = Box::new(MetricsFlusher::new(
            self.engines.as_ref().unwrap().engines.clone(),
        ));

        // Start metrics flusher
        if let Err(e) = metrics_flusher.start() {
            error!(%e; "failed to start metrics flusher");
        }

        self.to_stop.push(metrics_flusher);
    }

    fn run_server(&mut self, server_config: Arc<ServerConfig>) {
        let server = self.servers.as_mut().unwrap();
        server
            .server
            .build_and_bind()
            .unwrap_or_else(|e| fatal!("failed to build server: {}", e));
        server
            .server
            .start(server_config, self.security_mgr.clone())
            .unwrap_or_else(|e| fatal!("failed to start server: {}", e));
    }

    fn run_status_server(&mut self) {
        // Create a status server.
        let status_enabled =
            self.config.metric.address.is_empty() && !self.config.server.status_addr.is_empty();
        if status_enabled {
            let mut status_server = match StatusServer::new(
                self.config.server.status_thread_pool_size,
                Some(self.pd_client.clone()),
                self.cfg_controller.take().unwrap(),
                Arc::new(self.config.security.clone()),
                self.router.clone(),
            ) {
                Ok(status_server) => Box::new(status_server),
                Err(e) => {
                    error!(%e; "failed to start runtime for status service");
                    return;
                }
            };
            // Start the status server.
            if let Err(e) = status_server.start(
                self.config.server.status_addr.clone(),
                self.config.server.advertise_status_addr.clone(),
            ) {
                error!(%e; "failed to bind addr for status service");
            } else {
                self.to_stop.push(status_server);
            }
        }
    }

    fn stop(self) {
        let mut servers = self.servers.unwrap();
        servers
            .server
            .stop()
            .unwrap_or_else(|e| fatal!("failed to stop server: {}", e));

        servers.node.stop();
        self.region_info_accessor.stop();

        servers.lock_mgr.stop();

        self.to_stop.into_iter().for_each(|s| s.stop());
    }
}

impl TiKVServer<RocksEngine> {
    fn init_raw_engines(&mut self) -> Engines<RocksEngine, RocksEngine> {
        let env = get_env(self.encryption_key_manager.clone(), None /*base_env*/).unwrap();
        let block_cache = self.config.storage.block_cache.build_shared_cache();

        // Create raft engine.
        let raft_db_path = Path::new(&self.config.raft_store.raftdb_path);
        let config_raftdb = &self.config.raftdb;
        let mut raft_db_opts = config_raftdb.build_opt();
        raft_db_opts.set_env(env.clone());
        let raft_db_cf_opts = config_raftdb.build_cf_opts(&block_cache);
        let raft_engine = engine_rocks::raw_util::new_engine_opt(
            raft_db_path.to_str().unwrap(),
            raft_db_opts,
            raft_db_cf_opts,
        )
        .unwrap_or_else(|s| fatal!("failed to create raft engine: {}", s));

        // Create kv engine.
        let mut kv_db_opts = self.config.rocksdb.build_opt();
        kv_db_opts.set_env(env);
        kv_db_opts.add_event_listener(self.create_raftstore_compaction_listener());
        let kv_cfs_opts = self.config.rocksdb.build_cf_opts(&block_cache);
        let db_path = self
            .store_path
            .join(Path::new(storage::config::DEFAULT_ROCKSDB_SUB_DIR));
        let kv_engine = engine_rocks::raw_util::new_engine_opt(
            db_path.to_str().unwrap(),
            kv_db_opts,
            kv_cfs_opts,
        )
        .unwrap_or_else(|s| fatal!("failed to create kv engine: {}", s));

        let mut kv_engine = RocksEngine::from_db(Arc::new(kv_engine));
        let mut raft_engine = RocksEngine::from_db(Arc::new(raft_engine));
        let shared_block_cache = block_cache.is_some();
        kv_engine.set_shared_block_cache(shared_block_cache);
        raft_engine.set_shared_block_cache(shared_block_cache);
        let engines = Engines::new(kv_engine, raft_engine);

        let cfg_controller = self.cfg_controller.as_mut().unwrap();
        cfg_controller.register(
            tikv::config::Module::Rocksdb,
            Box::new(DBConfigManger::new(
                engines.kv.clone(),
                DBType::Kv,
                self.config.storage.block_cache.shared,
            )),
        );
        cfg_controller.register(
            tikv::config::Module::Raftdb,
            Box::new(DBConfigManger::new(
                engines.raft.clone(),
                DBType::Raft,
                self.config.storage.block_cache.shared,
            )),
        );

        engines
    }
}

impl TiKVServer<RaftLogEngine> {
    fn init_raw_engines(&mut self) -> Engines<RocksEngine, RaftLogEngine> {
        let env = get_env(self.encryption_key_manager.clone(), None /*base_env*/).unwrap();
        let block_cache = self.config.storage.block_cache.build_shared_cache();

        // Create raft engine.
        let raft_config = self.config.raft_engine.config();
        let raft_engine = RaftLogEngine::new(raft_config);

        // Create kv engine.
        let mut kv_db_opts = self.config.rocksdb.build_opt();
        kv_db_opts.set_env(env);
        kv_db_opts.add_event_listener(self.create_raftstore_compaction_listener());
        let kv_cfs_opts = self.config.rocksdb.build_cf_opts(&block_cache);
        let db_path = self
            .store_path
            .join(Path::new(storage::config::DEFAULT_ROCKSDB_SUB_DIR));
        let kv_engine = engine_rocks::raw_util::new_engine_opt(
            db_path.to_str().unwrap(),
            kv_db_opts,
            kv_cfs_opts,
        )
        .unwrap_or_else(|s| fatal!("failed to create kv engine: {}", s));

        let mut kv_engine = RocksEngine::from_db(Arc::new(kv_engine));
        let shared_block_cache = block_cache.is_some();
        kv_engine.set_shared_block_cache(shared_block_cache);
        let engines = Engines::new(kv_engine, raft_engine);

        let cfg_controller = self.cfg_controller.as_mut().unwrap();
        cfg_controller.register(
            tikv::config::Module::Rocksdb,
            Box::new(DBConfigManger::new(
                engines.kv.clone(),
                DBType::Kv,
                self.config.storage.block_cache.shared,
            )),
        );

        engines
    }
}

/// Various sanity-checks and logging before running a server.
///
/// Warnings are logged.
///
/// # Logs
///
/// The presence of these environment variables that affect the database
/// behavior is logged.
///
/// - `GRPC_POLL_STRATEGY`
/// - `http_proxy` and `https_proxy`
///
/// # Warnings
///
/// - if `net.core.somaxconn` < 32768
/// - if `net.ipv4.tcp_syncookies` is not 0
/// - if `vm.swappiness` is not 0
/// - if data directories are not on SSDs
/// - if the "TZ" environment variable is not set on unix
fn pre_start() {
    check_environment_variables();
    for e in tikv_util::config::check_kernel() {
        warn!(
            "check: kernel";
            "err" => %e
        );
    }
}

fn check_system_config(config: &TiKvConfig) {
    info!("beginning system configuration check");
    let mut rocksdb_max_open_files = config.rocksdb.max_open_files;
    if config.rocksdb.titan.enabled {
        // Titan engine maintains yet another pool of blob files and uses the same max
        // number of open files setup as rocksdb does. So we double the max required
        // open files here
        rocksdb_max_open_files *= 2;
    }
    if let Err(e) = tikv_util::config::check_max_open_fds(
        RESERVED_OPEN_FDS + (rocksdb_max_open_files + config.raftdb.max_open_files) as u64,
    ) {
        fatal!("{}", e);
    }

    // Check RocksDB data dir
    if let Err(e) = tikv_util::config::check_data_dir(&config.storage.data_dir) {
        warn!(
            "check: rocksdb-data-dir";
            "path" => &config.storage.data_dir,
            "err" => %e
        );
    }
    // Check raft data dir
    if let Err(e) = tikv_util::config::check_data_dir(&config.raft_store.raftdb_path) {
        warn!(
            "check: raftdb-path";
            "path" => &config.raft_store.raftdb_path,
            "err" => %e
        );
    }
}

fn try_lock_conflict_addr<P: AsRef<Path>>(path: P) -> File {
    let f = File::create(path.as_ref()).unwrap_or_else(|e| {
        fatal!(
            "failed to create lock at {}: {}",
            path.as_ref().display(),
            e
        )
    });

    if f.try_lock_exclusive().is_err() {
        fatal!(
            "{} already in use, maybe another instance is binding with this address.",
            path.as_ref().file_name().unwrap().to_str().unwrap()
        );
    }
    f
}

#[cfg(unix)]
fn get_lock_dir() -> String {
    format!("{}_TIKV_LOCK_FILES", unsafe { libc::getuid() })
}

#[cfg(not(unix))]
fn get_lock_dir() -> String {
    "TIKV_LOCK_FILES".to_owned()
}

/// A small trait for components which can be trivially stopped. Lets us keep
/// a list of these in `TiKV`, rather than storing each component individually.
trait Stop {
    fn stop(self: Box<Self>);
}

impl<E, R> Stop for StatusServer<E, R>
where
    E: 'static,
    R: 'static + Send,
{
    fn stop(self: Box<Self>) {
        (*self).stop()
    }
}

impl<ER: RaftEngine> Stop for MetricsFlusher<RocksEngine, ER> {
    fn stop(mut self: Box<Self>) {
        (*self).stop()
    }
}

impl<T: fmt::Display + Send + 'static> Stop for Worker<T> {
    fn stop(mut self: Box<Self>) {
        if let Some(Err(e)) = Worker::stop(&mut *self).map(JoinHandle::join) {
            info!(
                "ignore failure when stopping worker";
                "err" => ?e
            );
        }
    }
}
