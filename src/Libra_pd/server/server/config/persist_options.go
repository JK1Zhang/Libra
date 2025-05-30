// Copyright 2017 TiKV Project Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// See the License for the specific language governing permissions and
// limitations under the License.

package config

import (
	"reflect"
	"sync/atomic"
	"time"
	"unsafe"

	"github.com/coreos/go-semver/semver"
	"github.com/pingcap/kvproto/pkg/metapb"
	"github.com/tikv/pd/pkg/slice"
	"github.com/tikv/pd/pkg/typeutil"
	"github.com/tikv/pd/server/core"
	"github.com/tikv/pd/server/schedule/storelimit"
)

// PersistOptions wraps all configurations that need to persist to storage and
// allows to access them safely.
type PersistOptions struct {
	schedule        atomic.Value
	replication     atomic.Value
	pdServerConfig  atomic.Value
	replicationMode atomic.Value
	labelProperty   atomic.Value
	clusterVersion  unsafe.Pointer
}

// NewPersistOptions creates a new PersistOptions instance.
func NewPersistOptions(cfg *Config) *PersistOptions {
	o := &PersistOptions{}
	o.schedule.Store(&cfg.Schedule)
	o.replication.Store(&cfg.Replication)
	o.pdServerConfig.Store(&cfg.PDServerCfg)
	o.replicationMode.Store(&cfg.ReplicationMode)
	o.labelProperty.Store(cfg.LabelProperty)
	o.SetClusterVersion(&cfg.ClusterVersion)
	return o
}

// GetScheduleConfig returns scheduling configurations.
func (o *PersistOptions) GetScheduleConfig() *ScheduleConfig {
	return o.schedule.Load().(*ScheduleConfig)
}

// SetScheduleConfig sets the PD scheduling configuration.
func (o *PersistOptions) SetScheduleConfig(cfg *ScheduleConfig) {
	o.schedule.Store(cfg)
}

// GetReplicationConfig returns replication configurations.
func (o *PersistOptions) GetReplicationConfig() *ReplicationConfig {
	return o.replication.Load().(*ReplicationConfig)
}

// SetReplicationConfig sets the PD replication configuration.
func (o *PersistOptions) SetReplicationConfig(cfg *ReplicationConfig) {
	o.replication.Store(cfg)
}

// GetPDServerConfig returns pd server configurations.
func (o *PersistOptions) GetPDServerConfig() *PDServerConfig {
	return o.pdServerConfig.Load().(*PDServerConfig)
}

// SetPDServerConfig sets the PD configuration.
func (o *PersistOptions) SetPDServerConfig(cfg *PDServerConfig) {
	o.pdServerConfig.Store(cfg)
}

// GetReplicationModeConfig returns the replication mode config.
func (o *PersistOptions) GetReplicationModeConfig() *ReplicationModeConfig {
	return o.replicationMode.Load().(*ReplicationModeConfig)
}

// SetReplicationModeConfig sets the replication mode config.
func (o *PersistOptions) SetReplicationModeConfig(cfg *ReplicationModeConfig) {
	o.replicationMode.Store(cfg)
}

// GetLabelPropertyConfig returns the label property.
func (o *PersistOptions) GetLabelPropertyConfig() LabelPropertyConfig {
	return o.labelProperty.Load().(LabelPropertyConfig)
}

// SetLabelPropertyConfig sets the label property configuration.
func (o *PersistOptions) SetLabelPropertyConfig(cfg LabelPropertyConfig) {
	o.labelProperty.Store(cfg)
}

// GetClusterVersion returns the cluster version.
func (o *PersistOptions) GetClusterVersion() *semver.Version {
	return (*semver.Version)(atomic.LoadPointer(&o.clusterVersion))
}

// SetClusterVersion sets the cluster version.
func (o *PersistOptions) SetClusterVersion(v *semver.Version) {
	atomic.StorePointer(&o.clusterVersion, unsafe.Pointer(v))
}

// CASClusterVersion sets the cluster version.
func (o *PersistOptions) CASClusterVersion(old, new *semver.Version) bool {
	return atomic.CompareAndSwapPointer(&o.clusterVersion, unsafe.Pointer(old), unsafe.Pointer(new))
}

// GetLocationLabels returns the location labels for each region.
func (o *PersistOptions) GetLocationLabels() []string {
	return o.GetReplicationConfig().LocationLabels
}

// GetIsolationLevel returns the isolation label for each region.
func (o *PersistOptions) GetIsolationLevel() string {
	return o.GetReplicationConfig().IsolationLevel
}

// IsPlacementRulesEnabled returns if the placement rules is enabled.
func (o *PersistOptions) IsPlacementRulesEnabled() bool {
	return o.GetReplicationConfig().EnablePlacementRules
}

// GetStrictlyMatchLabel returns whether check label strict.
func (o *PersistOptions) GetStrictlyMatchLabel() bool {
	return o.GetReplicationConfig().StrictlyMatchLabel
}

// GetMaxReplicas returns the number of replicas for each region.
func (o *PersistOptions) GetMaxReplicas() int {
	return int(o.GetReplicationConfig().MaxReplicas)
}

// SetMaxReplicas sets the number of replicas for each region.
func (o *PersistOptions) SetMaxReplicas(replicas int) {
	v := o.GetReplicationConfig().Clone()
	v.MaxReplicas = uint64(replicas)
	o.SetReplicationConfig(v)
}

// GetMaxSnapshotCount returns the number of the max snapshot which is allowed to send.
func (o *PersistOptions) GetMaxSnapshotCount() uint64 {
	return o.GetScheduleConfig().MaxSnapshotCount
}

// GetMaxPendingPeerCount returns the number of the max pending peers.
func (o *PersistOptions) GetMaxPendingPeerCount() uint64 {
	return o.GetScheduleConfig().MaxPendingPeerCount
}

// GetMaxMergeRegionSize returns the max region size.
func (o *PersistOptions) GetMaxMergeRegionSize() uint64 {
	return o.GetScheduleConfig().MaxMergeRegionSize
}

// GetMaxMergeRegionKeys returns the max number of keys.
func (o *PersistOptions) GetMaxMergeRegionKeys() uint64 {
	return o.GetScheduleConfig().MaxMergeRegionKeys
}

// GetSplitMergeInterval returns the interval between finishing split and starting to merge.
func (o *PersistOptions) GetSplitMergeInterval() time.Duration {
	return o.GetScheduleConfig().SplitMergeInterval.Duration
}

// SetSplitMergeInterval to set the interval between finishing split and starting to merge. It's only used to test.
func (o *PersistOptions) SetSplitMergeInterval(splitMergeInterval time.Duration) {
	v := o.GetScheduleConfig().Clone()
	v.SplitMergeInterval = typeutil.Duration{Duration: splitMergeInterval}
	o.SetScheduleConfig(v)
}

// SetStoreLimit sets a store limit for a given type and rate.
func (o *PersistOptions) SetStoreLimit(storeID uint64, typ storelimit.Type, ratePerMin float64) {
	v := o.GetScheduleConfig().Clone()
	var sc StoreLimitConfig
	var rate float64
	switch typ {
	case storelimit.AddPeer:
		if _, ok := v.StoreLimit[storeID]; !ok {
			rate = DefaultStoreLimit.GetDefaultStoreLimit(storelimit.AddPeer)
		} else {
			rate = v.StoreLimit[storeID].RemovePeer
		}
		sc = StoreLimitConfig{AddPeer: ratePerMin, RemovePeer: rate}
	case storelimit.RemovePeer:
		if _, ok := v.StoreLimit[storeID]; !ok {
			rate = DefaultStoreLimit.GetDefaultStoreLimit(storelimit.RemovePeer)
		} else {
			rate = v.StoreLimit[storeID].AddPeer
		}
		sc = StoreLimitConfig{AddPeer: rate, RemovePeer: ratePerMin}
	}
	v.StoreLimit[storeID] = sc
	o.SetScheduleConfig(v)
}

// SetAllStoresLimit sets all store limit for a given type and rate.
func (o *PersistOptions) SetAllStoresLimit(typ storelimit.Type, ratePerMin float64) {
	v := o.GetScheduleConfig().Clone()
	switch typ {
	case storelimit.AddPeer:
		DefaultStoreLimit.SetDefaultStoreLimit(storelimit.AddPeer, ratePerMin)
		for storeID := range v.StoreLimit {
			sc := StoreLimitConfig{AddPeer: ratePerMin, RemovePeer: v.StoreLimit[storeID].RemovePeer}
			v.StoreLimit[storeID] = sc
		}
	case storelimit.RemovePeer:
		DefaultStoreLimit.SetDefaultStoreLimit(storelimit.RemovePeer, ratePerMin)
		for storeID := range v.StoreLimit {
			sc := StoreLimitConfig{AddPeer: v.StoreLimit[storeID].AddPeer, RemovePeer: ratePerMin}
			v.StoreLimit[storeID] = sc
		}
	}

	o.SetScheduleConfig(v)
}

// IsOneWayMergeEnabled returns if a region can only be merged into the next region of it.
func (o *PersistOptions) IsOneWayMergeEnabled() bool {
	return o.GetScheduleConfig().EnableOneWayMerge
}

// IsCrossTableMergeEnabled returns if across table merge is enabled.
func (o *PersistOptions) IsCrossTableMergeEnabled() bool {
	return o.GetScheduleConfig().EnableCrossTableMerge
}

// GetPatrolRegionInterval returns the interval of patrolling region.
func (o *PersistOptions) GetPatrolRegionInterval() time.Duration {
	return o.GetScheduleConfig().PatrolRegionInterval.Duration
}

// GetMaxStoreDownTime returns the max down time of a store.
func (o *PersistOptions) GetMaxStoreDownTime() time.Duration {
	return o.GetScheduleConfig().MaxStoreDownTime.Duration
}

// GetLeaderScheduleLimit returns the limit for leader schedule.
func (o *PersistOptions) GetLeaderScheduleLimit() uint64 {
	return o.GetScheduleConfig().LeaderScheduleLimit
}

// GetRegionScheduleLimit returns the limit for region schedule.
func (o *PersistOptions) GetRegionScheduleLimit() uint64 {
	return o.GetScheduleConfig().RegionScheduleLimit
}

// GetReplicaScheduleLimit returns the limit for replica schedule.
func (o *PersistOptions) GetReplicaScheduleLimit() uint64 {
	return o.GetScheduleConfig().ReplicaScheduleLimit
}

// GetMergeScheduleLimit returns the limit for merge schedule.
func (o *PersistOptions) GetMergeScheduleLimit() uint64 {
	return o.GetScheduleConfig().MergeScheduleLimit
}

// GetHotRegionScheduleLimit returns the limit for hot region schedule.
func (o *PersistOptions) GetHotRegionScheduleLimit() uint64 {
	return o.GetScheduleConfig().HotRegionScheduleLimit
}

// GetStoreLimit returns the limit of a store.
func (o *PersistOptions) GetStoreLimit(storeID uint64) StoreLimitConfig {
	if limit, ok := o.GetScheduleConfig().StoreLimit[storeID]; ok {
		return limit
	}
	cfg := o.GetScheduleConfig().Clone()
	sc := StoreLimitConfig{
		AddPeer:    DefaultStoreLimit.GetDefaultStoreLimit(storelimit.AddPeer),
		RemovePeer: DefaultStoreLimit.GetDefaultStoreLimit(storelimit.RemovePeer),
	}
	cfg.StoreLimit[storeID] = sc
	o.SetScheduleConfig(cfg)
	return o.GetScheduleConfig().StoreLimit[storeID]
}

// GetStoreLimitByType returns the limit of a store with a given type.
func (o *PersistOptions) GetStoreLimitByType(storeID uint64, typ storelimit.Type) float64 {
	limit := o.GetStoreLimit(storeID)
	switch typ {
	case storelimit.AddPeer:
		return limit.AddPeer
	case storelimit.RemovePeer:
		return limit.RemovePeer
	default:
		panic("no such limit type")
	}
}

// GetAllStoresLimit returns the limit of all stores.
func (o *PersistOptions) GetAllStoresLimit() map[uint64]StoreLimitConfig {
	return o.GetScheduleConfig().StoreLimit
}

// GetStoreLimitMode returns the limit mode of store.
func (o *PersistOptions) GetStoreLimitMode() string {
	return o.GetScheduleConfig().StoreLimitMode
}

// GetTolerantSizeRatio gets the tolerant size ratio.
func (o *PersistOptions) GetTolerantSizeRatio() float64 {
	return o.GetScheduleConfig().TolerantSizeRatio
}

// GetLowSpaceRatio returns the low space ratio.
func (o *PersistOptions) GetLowSpaceRatio() float64 {
	return o.GetScheduleConfig().LowSpaceRatio
}

// GetHighSpaceRatio returns the high space ratio.
func (o *PersistOptions) GetHighSpaceRatio() float64 {
	return o.GetScheduleConfig().HighSpaceRatio
}

// GetSchedulerMaxWaitingOperator returns the number of the max waiting operators.
func (o *PersistOptions) GetSchedulerMaxWaitingOperator() uint64 {
	return o.GetScheduleConfig().SchedulerMaxWaitingOperator
}

// GetLeaderSchedulePolicy is to get leader schedule policy.
func (o *PersistOptions) GetLeaderSchedulePolicy() core.SchedulePolicy {
	return core.StringToSchedulePolicy(o.GetScheduleConfig().LeaderSchedulePolicy)
}

// GetKeyType is to get key type.
func (o *PersistOptions) GetKeyType() core.KeyType {
	return core.StringToKeyType(o.GetPDServerConfig().KeyType)
}

// GetMaxResetTSGap gets the max gap to reset the tso.
func (o *PersistOptions) GetMaxResetTSGap() time.Duration {
	return o.GetPDServerConfig().MaxResetTSGap.Duration
}

// GetDashboardAddress gets dashboard address.
func (o *PersistOptions) GetDashboardAddress() string {
	return o.GetPDServerConfig().DashboardAddress
}

// IsUseRegionStorage returns if the independent region storage is enabled.
func (o *PersistOptions) IsUseRegionStorage() bool {
	return o.GetPDServerConfig().UseRegionStorage
}

// IsRemoveDownReplicaEnabled returns if remove down replica is enabled.
func (o *PersistOptions) IsRemoveDownReplicaEnabled() bool {
	return o.GetScheduleConfig().EnableRemoveDownReplica
}

// IsReplaceOfflineReplicaEnabled returns if replace offline replica is enabled.
func (o *PersistOptions) IsReplaceOfflineReplicaEnabled() bool {
	return o.GetScheduleConfig().EnableReplaceOfflineReplica
}

// IsMakeUpReplicaEnabled returns if make up replica is enabled.
func (o *PersistOptions) IsMakeUpReplicaEnabled() bool {
	return o.GetScheduleConfig().EnableMakeUpReplica
}

// IsRemoveExtraReplicaEnabled returns if remove extra replica is enabled.
func (o *PersistOptions) IsRemoveExtraReplicaEnabled() bool {
	return o.GetScheduleConfig().EnableRemoveExtraReplica
}

// IsLocationReplacementEnabled returns if location replace is enabled.
func (o *PersistOptions) IsLocationReplacementEnabled() bool {
	return o.GetScheduleConfig().EnableLocationReplacement
}

// IsDebugMetricsEnabled mocks method
func (o *PersistOptions) IsDebugMetricsEnabled() bool {
	return o.GetScheduleConfig().EnableDebugMetrics
}

// GetHotRegionCacheHitsThreshold is a threshold to decide if a region is hot.
func (o *PersistOptions) GetHotRegionCacheHitsThreshold() int {
	return int(o.GetScheduleConfig().HotRegionCacheHitsThreshold)
}

// GetHotSchedulerMode is used to control scheduling.
func (o *PersistOptions) GetHotSchedulerMode() int {
	return int(o.GetScheduleConfig().HotSchedulerMode)
}

// GetMultiHotSchedulerMode is used to control scheduling.
func (o *PersistOptions) GetMultiHotSchedulerMode() int {
	return int(o.GetScheduleConfig().MultiHotSchedulerMode)
}

// GetShuffleHotSchedulerMode is used to control scheduling.
func (o *PersistOptions) GetShuffleHotSchedulerMode() int {
	return int(o.GetScheduleConfig().ShuffleHotSchedulerMode)
}

// GetMultiScheduleIntervalMs is used to control scheduling rate.
func (o *PersistOptions) GetMultiScheduleIntervalMs() uint64 {
	return uint64(o.GetScheduleConfig().MultiScheduleIntervalMs)
}

// GetHotBalanceRatio is used to control scheduling.
func (o *PersistOptions) GetHotBalanceRatio() float64 {
	return float64(o.GetScheduleConfig().HotBalanceRatio)
}

// GetSchedulers gets the scheduler configurations.
func (o *PersistOptions) GetSchedulers() SchedulerConfigs {
	return o.GetScheduleConfig().Schedulers
}

// AddSchedulerCfg adds the scheduler configurations.
func (o *PersistOptions) AddSchedulerCfg(tp string, args []string) {
	v := o.GetScheduleConfig().Clone()
	for i, schedulerCfg := range v.Schedulers {
		// comparing args is to cover the case that there are schedulers in same type but not with same name
		// such as two schedulers of type "evict-leader",
		// one name is "evict-leader-scheduler-1" and the other is "evict-leader-scheduler-2"
		if reflect.DeepEqual(schedulerCfg, SchedulerConfig{Type: tp, Args: args, Disable: false}) {
			return
		}

		if reflect.DeepEqual(schedulerCfg, SchedulerConfig{Type: tp, Args: args, Disable: true}) {
			schedulerCfg.Disable = false
			v.Schedulers[i] = schedulerCfg
			o.SetScheduleConfig(v)
			return
		}
	}
	v.Schedulers = append(v.Schedulers, SchedulerConfig{Type: tp, Args: args, Disable: false})
	o.SetScheduleConfig(v)
}

// SetLabelProperty sets the label property.
func (o *PersistOptions) SetLabelProperty(typ, labelKey, labelValue string) {
	cfg := o.GetLabelPropertyConfig().Clone()
	for _, l := range cfg[typ] {
		if l.Key == labelKey && l.Value == labelValue {
			return
		}
	}
	cfg[typ] = append(cfg[typ], StoreLabel{Key: labelKey, Value: labelValue})
	o.labelProperty.Store(cfg)
}

// DeleteLabelProperty deletes the label property.
func (o *PersistOptions) DeleteLabelProperty(typ, labelKey, labelValue string) {
	cfg := o.GetLabelPropertyConfig().Clone()
	oldLabels := cfg[typ]
	cfg[typ] = []StoreLabel{}
	for _, l := range oldLabels {
		if l.Key == labelKey && l.Value == labelValue {
			continue
		}
		cfg[typ] = append(cfg[typ], l)
	}
	if len(cfg[typ]) == 0 {
		delete(cfg, typ)
	}
	o.labelProperty.Store(cfg)
}

// Persist saves the configuration to the storage.
func (o *PersistOptions) Persist(storage *core.Storage) error {
	cfg := &Config{
		Schedule:        *o.GetScheduleConfig(),
		Replication:     *o.GetReplicationConfig(),
		PDServerCfg:     *o.GetPDServerConfig(),
		ReplicationMode: *o.GetReplicationModeConfig(),
		LabelProperty:   o.GetLabelPropertyConfig(),
		ClusterVersion:  *o.GetClusterVersion(),
	}
	return storage.SaveConfig(cfg)
}

// Reload reloads the configuration from the storage.
func (o *PersistOptions) Reload(storage *core.Storage) error {
	cfg := &Config{}
	// pass nil to initialize cfg to default values (all items undefined)
	cfg.Adjust(nil)

	isExist, err := storage.LoadConfig(cfg)
	if err != nil {
		return err
	}
	o.adjustScheduleCfg(&cfg.Schedule)
	if isExist {
		o.schedule.Store(&cfg.Schedule)
		o.replication.Store(&cfg.Replication)
		o.pdServerConfig.Store(&cfg.PDServerCfg)
		o.replicationMode.Store(&cfg.ReplicationMode)
		o.labelProperty.Store(cfg.LabelProperty)
		o.SetClusterVersion(&cfg.ClusterVersion)
	}
	return nil
}

func (o *PersistOptions) adjustScheduleCfg(scheduleCfg *ScheduleConfig) {
	// In case we add new default schedulers.
	for _, ps := range DefaultSchedulers {
		if slice.NoneOf(scheduleCfg.Schedulers, func(i int) bool {
			return scheduleCfg.Schedulers[i].Type == ps.Type
		}) {
			scheduleCfg.Schedulers = append(scheduleCfg.Schedulers, ps)
		}
	}
	scheduleCfg.MigrateDeprecatedFlags()
}

// CheckLabelProperty checks the label property.
func (o *PersistOptions) CheckLabelProperty(typ string, labels []*metapb.StoreLabel) bool {
	pc := o.labelProperty.Load().(LabelPropertyConfig)
	for _, cfg := range pc[typ] {
		for _, l := range labels {
			if l.Key == cfg.Key && l.Value == cfg.Value {
				return true
			}
		}
	}
	return false
}
