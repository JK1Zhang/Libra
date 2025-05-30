// Copyright 2019 TiKV Project Authors.
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

package statistics

import (
	"sync"
	"time"

	"github.com/pingcap/kvproto/pkg/pdpb"
	"github.com/pingcap/log"
	"github.com/tikv/pd/server/core"
	"go.uber.org/zap"
)

// StoresStats is a cache hold hot regions.
type StoresStats struct {
	sync.RWMutex
	rollingStoresStats map[uint64]*RollingStoreStats
	bytesReadRate      float64
	bytesWriteRate     float64
	keysReadRate       float64
	keysWriteRate      float64
	opsRead            float64
	opsWrite           float64
}

// NewStoresStats creates a new hot spot cache.
func NewStoresStats() *StoresStats {
	return &StoresStats{
		rollingStoresStats: make(map[uint64]*RollingStoreStats),
	}
}

// CreateRollingStoreStats creates RollingStoreStats with a given store ID.
func (s *StoresStats) CreateRollingStoreStats(storeID uint64) {
	s.Lock()
	defer s.Unlock()
	s.rollingStoresStats[storeID] = newRollingStoreStats()
}

// RemoveRollingStoreStats removes RollingStoreStats with a given store ID.
func (s *StoresStats) RemoveRollingStoreStats(storeID uint64) {
	s.Lock()
	defer s.Unlock()
	delete(s.rollingStoresStats, storeID)
}

// GetRollingStoreStats gets RollingStoreStats with a given store ID.
func (s *StoresStats) GetRollingStoreStats(storeID uint64) *RollingStoreStats {
	s.RLock()
	defer s.RUnlock()
	return s.rollingStoresStats[storeID]
}

// GetOrCreateRollingStoreStats gets or creates RollingStoreStats with a given store ID.
func (s *StoresStats) GetOrCreateRollingStoreStats(storeID uint64) *RollingStoreStats {
	s.Lock()
	defer s.Unlock()
	ret, ok := s.rollingStoresStats[storeID]
	if !ok {
		ret = newRollingStoreStats()
		s.rollingStoresStats[storeID] = ret
	}
	return ret
}

// Observe records the current store status with a given store.
func (s *StoresStats) Observe(storeID uint64, stats *pdpb.StoreStats) {
	store := s.GetOrCreateRollingStoreStats(storeID)
	store.Observe(stats)
}

// Set sets the store statistics (for test).
func (s *StoresStats) Set(storeID uint64, stats *pdpb.StoreStats) {
	store := s.GetOrCreateRollingStoreStats(storeID)
	store.Set(stats)
}

// UpdateTotalBytesRate updates the total bytes write rate and read rate.
func (s *StoresStats) UpdateTotalBytesRate(f func() []*core.StoreInfo) {
	var totalBytesWriteRate float64
	var totalBytesReadRate float64
	var writeRate, readRate float64
	ss := f()
	s.RLock()
	defer s.RUnlock()
	for _, store := range ss {
		if store.IsUp() {
			stats, ok := s.rollingStoresStats[store.GetID()]
			if !ok {
				continue
			}
			writeRate, readRate = stats.GetBytesRate()
			totalBytesWriteRate += writeRate
			totalBytesReadRate += readRate
		}
	}
	s.bytesWriteRate = totalBytesWriteRate
	s.bytesReadRate = totalBytesReadRate
}

// UpdateTotalKeysRate updates the total keys write rate and read rate.
func (s *StoresStats) UpdateTotalKeysRate(f func() []*core.StoreInfo) {
	var totalKeysWriteRate float64
	var totalKeysReadRate float64
	var writeRate, readRate float64
	ss := f()
	s.RLock()
	defer s.RUnlock()
	for _, store := range ss {
		if store.IsUp() {
			stats, ok := s.rollingStoresStats[store.GetID()]
			if !ok {
				continue
			}
			writeRate, readRate = stats.GetKeysRate()
			totalKeysWriteRate += writeRate
			totalKeysReadRate += readRate
		}
	}
	s.keysWriteRate = totalKeysWriteRate
	s.keysReadRate = totalKeysReadRate
}

// UpdateTotalOps updates the total ops infos.
func (s *StoresStats) UpdateTotalOps(f func() []*core.StoreInfo) {
	var totalOpsRead float64
	var totalOpsWrite float64
	var opsRead, opsWrite float64
	ss := f()
	s.RLock()
	defer s.RUnlock()
	for _, store := range ss {
		if store.IsUp() {
			stats, ok := s.rollingStoresStats[store.GetID()]
			if !ok {
				continue
			}
			opsRead = stats.GetOpsRead()
			opsWrite = stats.GetOpsWrite()
			totalOpsRead += opsRead
			totalOpsWrite += opsWrite
		}
	}
	s.opsRead = totalOpsRead
	s.opsWrite = totalOpsWrite
}

// TotalBytesWriteRate returns the total written bytes rate of all StoreInfo.
func (s *StoresStats) TotalBytesWriteRate() float64 {
	return s.bytesWriteRate
}

// TotalBytesReadRate returns the total read bytes rate of all StoreInfo.
func (s *StoresStats) TotalBytesReadRate() float64 {
	return s.bytesReadRate
}

// TotalKeysWriteRate returns the total written keys rate of all StoreInfo.
func (s *StoresStats) TotalKeysWriteRate() float64 {
	return s.keysWriteRate
}

// TotalKeysReadRate returns the total read keys rate of all StoreInfo.
func (s *StoresStats) TotalKeysReadRate() float64 {
	return s.keysReadRate
}

// TotalOpsRead returns the total read ops of all StoreInfo.
func (s *StoresStats) TotalOpsRead() float64 {
	return s.opsRead
}

// TotalOpsWrite returns the total write ops of all StoreInfo.
func (s *StoresStats) TotalOpsWrite() float64 {
	return s.opsWrite
}

// GetStoreBytesRate returns the bytes write stat of the specified store.
func (s *StoresStats) GetStoreBytesRate(storeID uint64) (writeRate float64, readRate float64) {
	s.RLock()
	defer s.RUnlock()
	if storeStat, ok := s.rollingStoresStats[storeID]; ok {
		return storeStat.GetBytesRate()
	}
	return 0, 0
}

// GetStoreCPUUsage returns the total cpu usages of threads of the specified store.
func (s *StoresStats) GetStoreCPUUsage(storeID uint64) float64 {
	s.RLock()
	defer s.RUnlock()
	if storeStat, ok := s.rollingStoresStats[storeID]; ok {
		return storeStat.GetCPUUsage()
	}
	return 0
}

// GetStoreDiskReadRate returns the total read disk io rate of threads of the specified store.
func (s *StoresStats) GetStoreDiskReadRate(storeID uint64) float64 {
	s.RLock()
	defer s.RUnlock()
	if storeStat, ok := s.rollingStoresStats[storeID]; ok {
		return storeStat.GetDiskReadRate()
	}
	return 0
}

// GetStoreDiskWriteRate returns the total write disk io rate of threads of the specified store.
func (s *StoresStats) GetStoreDiskWriteRate(storeID uint64) float64 {
	s.RLock()
	defer s.RUnlock()
	if storeStat, ok := s.rollingStoresStats[storeID]; ok {
		return storeStat.GetDiskWriteRate()
	}
	return 0
}

// GetStoresCPUUsage returns the cpu usage stat of all StoreInfo.
func (s *StoresStats) GetStoresCPUUsage(cluster core.StoreSetInformer) map[uint64]float64 {
	return s.getStat(func(stats *RollingStoreStats) float64 {
		return stats.GetCPUUsage()
	})
}

// GetStoresDiskReadRate returns the disk read rate stat of all StoreInfo.
func (s *StoresStats) GetStoresDiskReadRate() map[uint64]float64 {
	return s.getStat(func(stats *RollingStoreStats) float64 {
		return stats.GetDiskReadRate()
	})
}

// GetStoresDiskWriteRate returns the disk write rate stat of all StoreInfo.
func (s *StoresStats) GetStoresDiskWriteRate() map[uint64]float64 {
	return s.getStat(func(stats *RollingStoreStats) float64 {
		return stats.GetDiskWriteRate()
	})
}

// GetStoreBytesWriteRate returns the bytes write stat of the specified store.
func (s *StoresStats) GetStoreBytesWriteRate(storeID uint64) float64 {
	s.RLock()
	defer s.RUnlock()
	if storeStat, ok := s.rollingStoresStats[storeID]; ok {
		return storeStat.GetBytesWriteRate()
	}
	return 0
}

// GetStoreBytesReadRate returns the bytes read stat of the specified store.
func (s *StoresStats) GetStoreBytesReadRate(storeID uint64) float64 {
	s.RLock()
	defer s.RUnlock()
	if storeStat, ok := s.rollingStoresStats[storeID]; ok {
		return storeStat.GetBytesReadRate()
	}
	return 0
}

// GetStoresBytesWriteStat returns the bytes write stat of all StoreInfo.
func (s *StoresStats) GetStoresBytesWriteStat() map[uint64]float64 {
	return s.getStat(func(stats *RollingStoreStats) float64 {
		return stats.GetBytesWriteRate()
	})
}

// GetStoresBytesWriteLeaderStat returns the bytes write leader stat of all StoreInfo.
func (s *StoresStats) GetStoresBytesWriteLeaderStat() map[uint64]float64 {
	return s.getStat(func(stats *RollingStoreStats) float64 {
		return stats.GetBytesWriteLeaderRate()
	})
}

// GetStoresBytesReadStat returns the bytes read stat of all StoreInfo.
func (s *StoresStats) GetStoresBytesReadStat() map[uint64]float64 {
	return s.getStat(func(stats *RollingStoreStats) float64 {
		return stats.GetBytesReadRate()
	})
}

// GetStoresKeysWriteStat returns the keys write stat of all StoreInfo.
func (s *StoresStats) GetStoresKeysWriteStat() map[uint64]float64 {
	return s.getStat(func(stats *RollingStoreStats) float64 {
		return stats.GetKeysWriteRate()
	})
}

// GetStoresKeysWriteLeaderStat returns the keys write leader stat of all StoreInfo.
func (s *StoresStats) GetStoresKeysWriteLeaderStat() map[uint64]float64 {
	return s.getStat(func(stats *RollingStoreStats) float64 {
		return stats.GetKeysWriteLeaderRate()
	})
}

// GetStoresKeysReadStat returns the bytes read stat of all StoreInfo.
func (s *StoresStats) GetStoresKeysReadStat() map[uint64]float64 {
	return s.getStat(func(stats *RollingStoreStats) float64 {
		return stats.GetKeysReadRate()
	})
}

// GetStoresOpsReadStat returns the read ops stat of all StoreInfo.
func (s *StoresStats) GetStoresOpsReadStat() map[uint64]float64 {
	return s.getStat(func(stats *RollingStoreStats) float64 {
		return stats.GetOpsRead()
	})
}

// GetStoresOpsWriteStat returns the write ops stat of all StoreInfo.
func (s *StoresStats) GetStoresOpsWriteStat() map[uint64]float64 {
	return s.getStat(func(stats *RollingStoreStats) float64 {
		return stats.GetOpsWrite()
	})
}

// GetStoresLoadsStat returns all of the load stats of all StoreInfo.
func (s *StoresStats) GetStoresLoadsStat() (ret []map[uint64]float64) {
	ret = append(ret,
		s.GetStoresBytesReadStat(),
		s.GetStoresKeysReadStat(),
		s.GetStoresOpsReadStat(),
		s.GetStoresBytesWriteLeaderStat(),
		s.GetStoresKeysWriteLeaderStat(),
		s.GetStoresOpsWriteStat(),
		s.GetStoresBytesWriteStat(),
		s.GetStoresKeysWriteStat(),
		s.GetStoresOpsWriteStat(),
	)
	return
}

func (s *StoresStats) getStat(getRate func(*RollingStoreStats) float64) map[uint64]float64 {
	s.RLock()
	defer s.RUnlock()
	res := make(map[uint64]float64, len(s.rollingStoresStats))
	for storeID, stats := range s.rollingStoresStats {
		res[storeID] = getRate(stats)
	}
	return res
}

func (s *StoresStats) storeIsUnhealthy(cluster core.StoreSetInformer, storeID uint64) bool {
	store := cluster.GetStore(storeID)
	return store.IsTombstone() || store.IsUnhealthy()
}

// FilterUnhealthyStore filter unhealthy store
func (s *StoresStats) FilterUnhealthyStore(cluster core.StoreSetInformer) {
	s.Lock()
	defer s.Unlock()
	for storeID := range s.rollingStoresStats {
		if s.storeIsUnhealthy(cluster, storeID) {
			delete(s.rollingStoresStats, storeID)
		}
	}
}

// RollingStoreStats are multiple sets of recent historical records with specified windows size.
type RollingStoreStats struct {
	sync.RWMutex
	bytesWriteRate          *TimeMedian
	bytesWriteLeaderRate    *TimeMedian
	bytesReadRate           *TimeMedian
	keysWriteRate           *TimeMedian
	keysWriteLeaderRate     *TimeMedian
	keysReadRate            *TimeMedian
	opsRead                 *TimeMedian
	opsWrite                *TimeMedian
	totalCPUUsage           MovingAvg
	totalBytesDiskReadRate  MovingAvg
	totalBytesDiskWriteRate MovingAvg
}

const (
	storeStatsRollingWindows = 3
	// DefaultAotSize is default size of average over time.
	DefaultAotSize = 2
	// DefaultWriteMfSize is default size of write median filter
	DefaultWriteMfSize = 5
	// DefaultReadMfSize is default size of read median filter
	DefaultReadMfSize = 3
)

// NewRollingStoreStats creates a RollingStoreStats.
func newRollingStoreStats() *RollingStoreStats {
	return &RollingStoreStats{
		bytesWriteRate:          NewTimeMedian(DefaultAotSize, DefaultWriteMfSize),
		bytesWriteLeaderRate:    NewTimeMedian(DefaultAotSize, DefaultWriteMfSize),
		bytesReadRate:           NewTimeMedian(DefaultAotSize, DefaultReadMfSize),
		keysWriteRate:           NewTimeMedian(DefaultAotSize, DefaultWriteMfSize),
		keysWriteLeaderRate:     NewTimeMedian(DefaultAotSize, DefaultWriteMfSize),
		keysReadRate:            NewTimeMedian(DefaultAotSize, DefaultReadMfSize),
		opsRead:                 NewTimeMedian(DefaultAotSize, DefaultReadMfSize),
		opsWrite:                NewTimeMedian(DefaultAotSize, DefaultReadMfSize),
		totalCPUUsage:           NewMedianFilter(storeStatsRollingWindows),
		totalBytesDiskReadRate:  NewMedianFilter(storeStatsRollingWindows),
		totalBytesDiskWriteRate: NewMedianFilter(storeStatsRollingWindows),
	}
}

func collect(records []*pdpb.RecordPair) float64 {
	var total uint64
	for _, record := range records {
		total += record.GetValue()
	}
	return float64(total)
}

// Observe records current statistics.
func (r *RollingStoreStats) Observe(stats *pdpb.StoreStats) {
	statInterval := stats.GetInterval()
	interval := statInterval.GetEndTimestamp() - statInterval.GetStartTimestamp()
	log.Debug("update store stats", zap.Uint64("key-write", stats.KeysWritten), zap.Uint64("bytes-write", stats.BytesWritten), zap.Duration("interval", time.Duration(interval)*time.Second), zap.Uint64("store-id", stats.GetStoreId()))
	r.Lock()
	defer r.Unlock()
	r.bytesWriteRate.Add(float64(stats.BytesWritten), time.Duration(interval)*time.Second)
	r.bytesWriteLeaderRate.Add(float64(stats.LeaderBytesWritten), time.Duration(interval)*time.Second)
	r.bytesReadRate.Add(float64(stats.BytesRead), time.Duration(interval)*time.Second)
	r.keysWriteRate.Add(float64(stats.KeysWritten), time.Duration(interval)*time.Second)
	r.keysWriteLeaderRate.Add(float64(stats.LeaderKeysWritten), time.Duration(interval)*time.Second)
	r.keysReadRate.Add(float64(stats.KeysRead), time.Duration(interval)*time.Second)
	r.opsRead.Add(float64(stats.OpsRead), time.Duration(interval)*time.Second)
	r.opsWrite.Add(float64(stats.OpsWrite), time.Duration(interval)*time.Second)

	// Updates the cpu usages and disk rw rates of store.
	r.totalCPUUsage.Add(collect(stats.GetCpuUsages()))
	r.totalBytesDiskReadRate.Add(collect(stats.GetReadIoRates()))
	r.totalBytesDiskWriteRate.Add(collect(stats.GetWriteIoRates()))
}

// Set sets the statistics (for test).
func (r *RollingStoreStats) Set(stats *pdpb.StoreStats) {
	statInterval := stats.GetInterval()
	interval := statInterval.GetEndTimestamp() - statInterval.GetStartTimestamp()
	if interval == 0 {
		return
	}
	r.Lock()
	defer r.Unlock()
	r.bytesWriteRate.Set(float64(stats.BytesWritten) / float64(interval))
	r.bytesWriteLeaderRate.Set(float64(stats.LeaderBytesWritten) / float64(interval))
	r.bytesReadRate.Set(float64(stats.BytesRead) / float64(interval))
	r.keysWriteRate.Set(float64(stats.KeysWritten) / float64(interval))
	r.keysWriteLeaderRate.Set(float64(stats.LeaderKeysWritten) / float64(interval))
	r.keysReadRate.Set(float64(stats.KeysRead) / float64(interval))
	r.opsRead.Set(float64(stats.OpsRead) / float64(interval))
	r.opsWrite.Set(float64(stats.OpsWrite) / float64(interval))
}

// GetBytesRate returns the bytes write rate and the bytes read rate.
func (r *RollingStoreStats) GetBytesRate() (writeRate float64, readRate float64) {
	r.RLock()
	defer r.RUnlock()
	return r.bytesWriteRate.Get(), r.bytesReadRate.Get()
}

// GetBytesWriteRate returns the bytes write rate.
func (r *RollingStoreStats) GetBytesWriteRate() float64 {
	r.RLock()
	defer r.RUnlock()
	return r.bytesWriteRate.Get()
}

// GetBytesWriteLeaderRate returns the bytes write leader rate.
func (r *RollingStoreStats) GetBytesWriteLeaderRate() float64 {
	r.RLock()
	defer r.RUnlock()
	return r.bytesWriteLeaderRate.Get()
}

// GetBytesReadRate returns the bytes read rate.
func (r *RollingStoreStats) GetBytesReadRate() float64 {
	r.RLock()
	defer r.RUnlock()
	return r.bytesReadRate.Get()
}

// GetKeysRate returns the keys write rate and the keys read rate.
func (r *RollingStoreStats) GetKeysRate() (writeRate float64, readRate float64) {
	r.RLock()
	defer r.RUnlock()
	return r.keysWriteRate.Get(), r.keysReadRate.Get()
}

// GetKeysWriteRate returns the keys write rate.
func (r *RollingStoreStats) GetKeysWriteRate() float64 {
	r.RLock()
	defer r.RUnlock()
	return r.keysWriteRate.Get()
}

// GetKeysWriteLeaderRate returns the keys write leader rate.
func (r *RollingStoreStats) GetKeysWriteLeaderRate() float64 {
	r.RLock()
	defer r.RUnlock()
	return r.keysWriteLeaderRate.Get()
}

// GetKeysReadRate returns the keys read rate.
func (r *RollingStoreStats) GetKeysReadRate() float64 {
	r.RLock()
	defer r.RUnlock()
	return r.keysReadRate.Get()
}

// GetOpsRead returns the read ops.
func (r *RollingStoreStats) GetOpsRead() float64 {
	r.RLock()
	defer r.RUnlock()
	return r.opsRead.Get()
}

// GetOpsWrite returns the write ops.
func (r *RollingStoreStats) GetOpsWrite() float64 {
	r.RLock()
	defer r.RUnlock()
	return r.opsWrite.Get()
}

// GetCPUUsage returns the total cpu usages of threads in the store.
func (r *RollingStoreStats) GetCPUUsage() float64 {
	r.RLock()
	defer r.RUnlock()
	return r.totalCPUUsage.Get()
}

// GetDiskReadRate returns the total read disk io rate of threads in the store.
func (r *RollingStoreStats) GetDiskReadRate() float64 {
	r.RLock()
	defer r.RUnlock()
	return r.totalBytesDiskReadRate.Get()
}

// GetDiskWriteRate returns the total write disk io rate of threads in the store.
func (r *RollingStoreStats) GetDiskWriteRate() float64 {
	r.RLock()
	defer r.RUnlock()
	return r.totalBytesDiskWriteRate.Get()
}
