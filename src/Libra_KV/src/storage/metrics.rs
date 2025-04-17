// Copyright 2016 TiKV Project Authors. Licensed under Apache-2.0.

use prometheus::*;
use prometheus_static_metric::*;

use std::cell::RefCell;
use std::mem;
use std::time::Instant;
use std::sync::mpsc::Sender;

use crate::server::metrics::{GcKeysCF as ServerGcKeysCF, GcKeysDetail as ServerGcKeysDetail};
use crate::storage::kv::{FlowStatsReporter, Statistics};
use kvproto::kvrpcpb::KeyRange;
use kvproto::metapb;
use raftstore::store::util::build_key_range;
use raftstore::store::{ReadStats, RequestInfo};
use tikv_util::collections::HashMap;

struct StorageLocalMetrics {
    local_scan_details: HashMap<CommandKind, Statistics>,
    local_read_stats: ReadStats,
    local_write_stats: ReadStats,
    local_last_update_write_time: Instant,
}

thread_local! {
    static TLS_STORAGE_METRICS: RefCell<StorageLocalMetrics> = RefCell::new(
        StorageLocalMetrics {
            local_scan_details: HashMap::default(),
            local_read_stats:ReadStats::default(),
            local_write_stats:ReadStats::default_write(),
            local_last_update_write_time: Instant::now(),
        }
    );
}

pub fn tls_flush<R: FlowStatsReporter>(reporter: &R) {
    TLS_STORAGE_METRICS.with(|m| {
        let mut m = m.borrow_mut();

        for (cmd, stat) in m.local_scan_details.drain() {
            for (cf, cf_details) in stat.details_enum().iter() {
                for (tag, count) in cf_details.iter() {
                    KV_COMMAND_SCAN_DETAILS_STATIC
                        .get(cmd)
                        .get((*cf).into())
                        .get((*tag).into())
                        .inc_by(*count as i64);
                }
            }
        }

        // Report PD metrics
        if !m.local_read_stats.is_empty() {
            let mut read_stats = ReadStats::default();
            mem::swap(&mut read_stats, &mut m.local_read_stats);
            reporter.report_read_stats(read_stats);
        }
    });
}

pub fn tls_flush_write<R: FlowStatsReporter>(reporter: &Option<R>, write_stats: ReadStats) {
    TLS_STORAGE_METRICS.with(|_| {
        match reporter {
            Some(rep) => {
                rep.report_write_stats(write_stats);
            }
            None => {}
        };
        
    });
}

pub fn tls_collect_scan_details(cmd: CommandKind, stats: &Statistics) {
    TLS_STORAGE_METRICS.with(|m| {
        m.borrow_mut()
            .local_scan_details
            .entry(cmd)
            .or_insert_with(Default::default)
            .add(stats);
    });
}

pub fn tls_collect_read_flow(region_id: u64, statistics: &Statistics) {
    TLS_STORAGE_METRICS.with(|m| {
        let mut m = m.borrow_mut();
        m.local_read_stats.add_flow(
            region_id,
            &statistics.write.flow_stats,
            &statistics.data.flow_stats,
        );
    });
}

pub fn tls_collect_qps(
    region_id: u64,
    peer: &metapb::Peer,
    start_key: &[u8],
    end_key: &[u8],
    reverse_scan: bool,
) {
    TLS_STORAGE_METRICS.with(|m| {
        let mut m = m.borrow_mut();
        let key_range = build_key_range(start_key, end_key, reverse_scan);
        m.local_read_stats.add_qps(region_id, peer, key_range);
    });
}

pub fn tls_collect_qps_batch(region_id: u64, peer: &metapb::Peer, key_ranges: Vec<KeyRange>) {
    TLS_STORAGE_METRICS.with(|m| {
        let mut m = m.borrow_mut();
        m.local_read_stats
            .add_qps_batch(region_id, peer, key_ranges);
    });
}

pub fn tls_collect_req_info(
    region_id: u64,
    peer: &metapb::Peer,
    mut req_info: RequestInfo,
    statistics: &Statistics,
) {
    TLS_STORAGE_METRICS.with(|m| {
        if req_info.start_key.is_empty() && req_info.end_key.is_empty() {
            return;
        }
        let mut m = m.borrow_mut();
        req_info.bytes = statistics.total_read_bytes();
        req_info.keys = statistics.total_read_keys();
        m.local_read_stats.add_req_info(region_id, peer, req_info);
    });
}

pub fn tls_collect_req_info_batch(region_id: u64, peer: &metapb::Peer, mut req_infos: Vec<RequestInfo>, statistics: &Statistics) {
    TLS_STORAGE_METRICS.with(|m| {
        if req_infos.is_empty() {
            return;
        }
        let mut m = m.borrow_mut();
        let avg_bytes = statistics.total_read_bytes() / req_infos.len();
        let avg_keys = statistics.total_read_keys() / req_infos.len();
        for req_info in &mut req_infos {
            req_info.bytes = avg_bytes;
            req_info.keys = avg_keys;
        }
        m.local_read_stats
            .add_req_info_batch(region_id, peer, req_infos);
    });
}

pub fn tls_collect_write_req_info(
    sender: &Option<Sender<ReadStats>>,
    region_id: u64,
    peer: &metapb::Peer,
    mut req_info: RequestInfo,
    write_size: usize,
) {
    TLS_STORAGE_METRICS.with(|m| {
        if req_info.start_key.is_empty() && req_info.end_key.is_empty() {
            return;
        }
        let mut m = m.borrow_mut();
        req_info.bytes = write_size;
        req_info.keys = 1;
        m.local_write_stats.add_req_info(region_id, peer, req_info);

        if (Instant::now() - m.local_last_update_write_time).as_millis() > 1000 {
            let mut write_stats = ReadStats::default_write();
            mem::swap(&mut write_stats, &mut m.local_write_stats);
            if let Some(s) = sender {
                if s.send(write_stats).is_err() {
                    warn!("send write_stats failed, are we shutting down?")
                }
            }
            m.local_last_update_write_time = Instant::now();
        }
    });
}

make_auto_flush_static_metric! {
    pub label_enum CommandKind {
        get,
        raw_batch_get_command,
        scan,
        batch_get,
        batch_get_command,
        prewrite,
        acquire_pessimistic_lock,
        commit,
        cleanup,
        rollback,
        pessimistic_rollback,
        txn_heart_beat,
        check_txn_status,
        check_secondary_locks,
        scan_lock,
        resolve_lock,
        resolve_lock_lite,
        delete_range,
        pause,
        key_mvcc,
        start_ts_mvcc,
        raw_get,
        raw_batch_get,
        raw_scan,
        raw_batch_scan,
        raw_put,
        raw_batch_put,
        raw_delete,
        raw_delete_range,
        raw_batch_delete,
    }

    pub label_enum CommandStageKind {
        new,
        snapshot,
        async_snapshot_err,
        snapshot_ok,
        snapshot_err,
        read_finish,
        next_cmd,
        lock_wait,
        process,
        prepare_write_err,
        write,
        write_finish,
        async_write_err,
        error,
        pipelined_write,
        pipelined_write_finish,
    }

    pub label_enum CommandPriority {
        low,
        normal,
        high,
    }

    pub label_enum GcKeysCF {
        default,
        lock,
        write,
    }

    pub label_enum GcKeysDetail {
        processed_keys,
        get,
        next,
        prev,
        seek,
        seek_for_prev,
        over_seek_bound,
        next_tombstone,
        prev_tombstone,
        seek_tombstone,
        seek_for_prev_tombstone,
    }

    pub struct CommandScanDetails: LocalIntCounter {
        "req" => CommandKind,
        "cf" => GcKeysCF,
        "tag" => GcKeysDetail,
    }

    pub struct SchedDurationVec: LocalHistogram {
        "type" => CommandKind,
    }

    pub struct ProcessingReadVec: LocalHistogram {
        "type" => CommandKind,
    }

    pub struct KReadVec: LocalHistogram {
        "type" => CommandKind,
    }

    pub struct KvCommandCounterVec: LocalIntCounter {
        "type" => CommandKind,
    }

    pub struct SchedStageCounterVec: LocalIntCounter {
        "type" => CommandKind,
        "stage" => CommandStageKind,
    }

    pub struct SchedLatchDurationVec: LocalHistogram {
        "type" => CommandKind,
    }

    pub struct KvCommandKeysWrittenVec: LocalHistogram {
        "type" => CommandKind,
    }

    pub struct SchedTooBusyVec: LocalIntCounter {
        "type" => CommandKind,
    }

    pub struct SchedCommandPriCounterVec: LocalIntCounter {
        "priority" => CommandPriority,
    }
}

impl Into<GcKeysCF> for ServerGcKeysCF {
    fn into(self) -> GcKeysCF {
        match self {
            ServerGcKeysCF::default => GcKeysCF::default,
            ServerGcKeysCF::lock => GcKeysCF::lock,
            ServerGcKeysCF::write => GcKeysCF::write,
        }
    }
}

impl Into<GcKeysDetail> for ServerGcKeysDetail {
    fn into(self) -> GcKeysDetail {
        match self {
            ServerGcKeysDetail::processed_keys => GcKeysDetail::processed_keys,
            ServerGcKeysDetail::get => GcKeysDetail::get,
            ServerGcKeysDetail::next => GcKeysDetail::next,
            ServerGcKeysDetail::prev => GcKeysDetail::prev,
            ServerGcKeysDetail::seek => GcKeysDetail::seek,
            ServerGcKeysDetail::seek_for_prev => GcKeysDetail::seek_for_prev,
            ServerGcKeysDetail::over_seek_bound => GcKeysDetail::over_seek_bound,
            ServerGcKeysDetail::next_tombstone => GcKeysDetail::next_tombstone,
            ServerGcKeysDetail::prev_tombstone => GcKeysDetail::prev_tombstone,
            ServerGcKeysDetail::seek_tombstone => GcKeysDetail::seek_tombstone,
            ServerGcKeysDetail::seek_for_prev_tombstone => GcKeysDetail::seek_for_prev_tombstone,
        }
    }
}

lazy_static! {
    pub static ref KV_COMMAND_COUNTER_VEC: IntCounterVec = register_int_counter_vec!(
        "tikv_storage_command_total",
        "Total number of commands received.",
        &["type"]
    )
    .unwrap();
    pub static ref KV_COMMAND_COUNTER_VEC_STATIC: KvCommandCounterVec =
        auto_flush_from!(KV_COMMAND_COUNTER_VEC, KvCommandCounterVec);
    pub static ref SCHED_STAGE_COUNTER: IntCounterVec = {
        register_int_counter_vec!(
            "tikv_scheduler_stage_total",
            "Total number of commands on each stage.",
            &["type", "stage"]
        )
        .unwrap()
    };
    pub static ref SCHED_STAGE_COUNTER_VEC: SchedStageCounterVec =
        auto_flush_from!(SCHED_STAGE_COUNTER, SchedStageCounterVec);
    pub static ref SCHED_WRITING_BYTES_GAUGE: IntGauge = register_int_gauge!(
        "tikv_scheduler_writing_bytes",
        "Total number of writing kv."
    )
    .unwrap();
    pub static ref SCHED_CONTEX_GAUGE: IntGauge = register_int_gauge!(
        "tikv_scheduler_contex_total",
        "Total number of pending commands."
    )
    .unwrap();
    pub static ref SCHED_HISTOGRAM_VEC: HistogramVec = register_histogram_vec!(
        "tikv_scheduler_command_duration_seconds",
        "Bucketed histogram of command execution",
        &["type"],
        exponential_buckets(0.0005, 2.0, 20).unwrap()
    )
    .unwrap();
    pub static ref SCHED_HISTOGRAM_VEC_STATIC: SchedDurationVec =
        auto_flush_from!(SCHED_HISTOGRAM_VEC, SchedDurationVec);
    pub static ref SCHED_LATCH_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "tikv_scheduler_latch_wait_duration_seconds",
        "Bucketed histogram of latch wait",
        &["type"],
        exponential_buckets(0.0005, 2.0, 20).unwrap()
    )
    .unwrap();
    pub static ref SCHED_LATCH_HISTOGRAM_VEC: SchedLatchDurationVec =
        auto_flush_from!(SCHED_LATCH_HISTOGRAM, SchedLatchDurationVec);
    pub static ref SCHED_PROCESSING_READ_HISTOGRAM_VEC: HistogramVec = register_histogram_vec!(
        "tikv_scheduler_processing_read_duration_seconds",
        "Bucketed histogram of processing read duration",
        &["type"],
        exponential_buckets(0.0005, 2.0, 20).unwrap()
    )
    .unwrap();
    pub static ref SCHED_PROCESSING_READ_HISTOGRAM_STATIC: ProcessingReadVec =
        auto_flush_from!(SCHED_PROCESSING_READ_HISTOGRAM_VEC, ProcessingReadVec);
    pub static ref SCHED_PROCESSING_WRITE_HISTOGRAM_VEC: HistogramVec = register_histogram_vec!(
        "tikv_scheduler_processing_write_duration_seconds",
        "Bucketed histogram of processing write duration",
        &["type"],
        exponential_buckets(0.0005, 2.0, 20).unwrap()
    )
    .unwrap();
    pub static ref SCHED_TOO_BUSY_COUNTER: IntCounterVec = register_int_counter_vec!(
        "tikv_scheduler_too_busy_total",
        "Total count of scheduler too busy",
        &["type"]
    )
    .unwrap();
    pub static ref SCHED_TOO_BUSY_COUNTER_VEC: SchedTooBusyVec =
        auto_flush_from!(SCHED_TOO_BUSY_COUNTER, SchedTooBusyVec);
    pub static ref SCHED_COMMANDS_PRI_COUNTER_VEC: IntCounterVec = register_int_counter_vec!(
        "tikv_scheduler_commands_pri_total",
        "Total count of different priority commands",
        &["priority"]
    )
    .unwrap();
    pub static ref SCHED_COMMANDS_PRI_COUNTER_VEC_STATIC: SchedCommandPriCounterVec =
        auto_flush_from!(SCHED_COMMANDS_PRI_COUNTER_VEC, SchedCommandPriCounterVec);
    pub static ref KV_COMMAND_KEYREAD_HISTOGRAM_VEC: HistogramVec = register_histogram_vec!(
        "tikv_scheduler_kv_command_key_read",
        "Bucketed histogram of keys read of a kv command",
        &["type"],
        exponential_buckets(1.0, 2.0, 21).unwrap()
    )
    .unwrap();
    pub static ref KV_COMMAND_KEYREAD_HISTOGRAM_STATIC: KReadVec =
        auto_flush_from!(KV_COMMAND_KEYREAD_HISTOGRAM_VEC, KReadVec);
    pub static ref KV_COMMAND_SCAN_DETAILS: IntCounterVec = register_int_counter_vec!(
        "tikv_scheduler_kv_scan_details",
        "Bucketed counter of kv keys scan details for each cf",
        &["req", "cf", "tag"]
    )
    .unwrap();
    pub static ref KV_COMMAND_SCAN_DETAILS_STATIC: CommandScanDetails =
        auto_flush_from!(KV_COMMAND_SCAN_DETAILS, CommandScanDetails);
    pub static ref KV_COMMAND_KEYWRITE_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "tikv_scheduler_kv_command_key_write",
        "Bucketed histogram of keys write of a kv command",
        &["type"],
        exponential_buckets(1.0, 2.0, 21).unwrap()
    )
    .unwrap();
    pub static ref KV_COMMAND_KEYWRITE_HISTOGRAM_VEC: KvCommandKeysWrittenVec =
        auto_flush_from!(KV_COMMAND_KEYWRITE_HISTOGRAM, KvCommandKeysWrittenVec);
    pub static ref REQUEST_EXCEED_BOUND: IntCounter = register_int_counter!(
        "tikv_request_exceed_bound",
        "Counter of request exceed bound"
    )
    .unwrap();
}
