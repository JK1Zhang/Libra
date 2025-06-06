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

import "time"

const (
	byteDim int = iota
	keyDim
	opsDim
	otherByteDim
	otherKeyDim
	otherOpsDim
	dimLen
)

// HotPeerStat records each hot peer's statistics
type HotPeerStat struct {
	StoreID  uint64 `json:"store_id"`
	RegionID uint64 `json:"region_id"`

	// HotDegree records the times for the region considered as hot spot during each HandleRegionHeartbeat
	HotDegree int `json:"hot_degree"`
	// AntiCount used to eliminate some noise when remove region in cache
	AntiCount int `json:"anti_count"`

	Kind     FlowKind `json:"kind"`
	ByteRate float64  `json:"flow_bytes"`
	KeyRate  float64  `json:"flow_keys"`
	Ops      float64  `json:"flow_ops"`

	OtherByteRate float64 `json:"other_flow_bytes"`
	OtherKeyRate  float64 `json:"other_flow_keys"`
	OtherOps      float64 `json:"other_flow_ops"`

	// rolling statistics, recording some recently added records.
	rollingByteRate MovingAvg
	rollingKeyRate  MovingAvg
	rollingOps      MovingAvg

	rollingOtherByteRate MovingAvg
	rollingOtherKeyRate  MovingAvg
	rollingOtherOps      MovingAvg

	// LastUpdateTime used to calculate average write
	LastUpdateTime time.Time `json:"last_update_time"`
	// Version used to check the region split times
	Version uint64 `json:"version"`

	needDelete bool
	isLeader   bool
	isNew      bool
}

// ID returns region ID. Implementing TopNItem.
func (stat *HotPeerStat) ID() uint64 {
	return stat.RegionID
}

// Less compares two HotPeerStat.Implementing TopNItem.
func (stat *HotPeerStat) Less(k int, than TopNItem) bool {
	rhs := than.(*HotPeerStat)
	switch k {
	case keyDim:
		return stat.GetKeyRate() < rhs.GetKeyRate()
	case byteDim:
		return stat.GetByteRate() < rhs.GetByteRate()
	case opsDim:
		fallthrough
	default:
		return stat.GetOps() < rhs.GetOps()
	}
}

// IsNeedDelete to delete the item in cache.
func (stat *HotPeerStat) IsNeedDelete() bool {
	return stat.needDelete
}

// IsLeader indicates the item belong to the leader.
func (stat *HotPeerStat) IsLeader() bool {
	return stat.isLeader
}

// IsNew indicates the item is first update in the cache of the region.
func (stat *HotPeerStat) IsNew() bool {
	return stat.isNew
}

// GetByteRate returns denoised BytesRate if possible.
func (stat *HotPeerStat) GetByteRate() float64 {
	if stat.rollingByteRate == nil {
		return stat.ByteRate
	}
	return stat.rollingByteRate.Get()
}

// GetKeyRate returns denoised KeysRate if possible.
func (stat *HotPeerStat) GetKeyRate() float64 {
	if stat.rollingKeyRate == nil {
		return stat.KeyRate
	}
	return stat.rollingKeyRate.Get()
}

// GetOps returns denoised ops if possible.
func (stat *HotPeerStat) GetOps() float64 {
	if stat.rollingOps == nil {
		return stat.Ops
	}
	return stat.rollingOps.Get()
}

// GetOtherByteRate returns denoised BytesRate if possible.
func (stat *HotPeerStat) GetOtherByteRate() float64 {
	if stat.rollingOtherByteRate == nil {
		return stat.OtherByteRate
	}
	return stat.rollingOtherByteRate.Get()
}

// GetOtherKeyRate returns denoised KeysRate if possible.
func (stat *HotPeerStat) GetOtherKeyRate() float64 {
	if stat.rollingOtherKeyRate == nil {
		return stat.OtherKeyRate
	}
	return stat.rollingOtherKeyRate.Get()
}

// GetOtherOps returns denoised ops if possible.
func (stat *HotPeerStat) GetOtherOps() float64 {
	if stat.rollingOtherOps == nil {
		return stat.OtherOps
	}
	return stat.rollingOtherOps.Get()
}

// GetLoads returns all of the loads if possible.
func (stat *HotPeerStat) GetLoads() (loads []float64) {
	if stat.Kind == ReadFlow {
		loads = append(loads,
			stat.GetByteRate(), stat.GetKeyRate(), stat.GetOps(),
			stat.GetOtherByteRate(), stat.GetOtherKeyRate(), stat.GetOtherOps(),
			stat.GetOtherByteRate(), stat.GetOtherKeyRate(), stat.GetOtherOps(),
		)
	} else {
		loads = append(loads,
			stat.GetOtherByteRate(), stat.GetOtherKeyRate(), stat.GetOtherOps(),
			stat.GetByteRate(), stat.GetKeyRate(), stat.GetOps(),
			stat.GetByteRate(), stat.GetKeyRate(), stat.GetOps(),
		)
	}
	return
}

// Clone clones the HotPeerStat
func (stat *HotPeerStat) Clone() *HotPeerStat {
	ret := *stat
	ret.ByteRate = stat.GetByteRate()
	ret.rollingByteRate = nil
	ret.KeyRate = stat.GetKeyRate()
	ret.rollingKeyRate = nil
	ret.Ops = stat.GetOps()
	ret.rollingOps = nil

	ret.OtherByteRate = stat.GetOtherByteRate()
	ret.rollingOtherByteRate = nil
	ret.OtherKeyRate = stat.GetOtherKeyRate()
	ret.rollingOtherKeyRate = nil
	ret.OtherOps = stat.GetOtherOps()
	ret.rollingOtherOps = nil
	return &ret
}
