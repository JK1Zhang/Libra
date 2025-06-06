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
	"time"

	"github.com/pingcap/kvproto/pkg/metapb"
	"github.com/tikv/pd/server/core"
)

const (
	topNN             = 60
	topNTTL           = 3 * RegionHeartBeatReportInterval * time.Second
	hotThresholdRatio = 0.8

	rollingWindowsSize = 5

	hotRegionReportMinInterval = 3

	hotRegionAntiCount = 2

	updateWithOtherStats = true
)

var (
	minHotThresholds = [2][dimLen]float64{
		WriteFlow: {
			byteDim:      256,
			keyDim:       16,
			opsDim:       16,
			otherByteDim: 256,
			otherKeyDim:  16,
			otherOpsDim:  16,
		},
		ReadFlow: {
			byteDim:      256,
			keyDim:       16,
			opsDim:       16,
			otherByteDim: 256,
			otherKeyDim:  16,
			otherOpsDim:  16,
		},
	}
)

// hotPeerCache saves the hot peer's statistics.
type hotPeerCache struct {
	kind           FlowKind
	peersOfStore   map[uint64]*TopN               // storeID -> hot peers
	storesOfRegion map[uint64]map[uint64]struct{} // regionID -> storeIDs
}

// NewHotStoresStats creates a HotStoresStats
func NewHotStoresStats(kind FlowKind) *hotPeerCache {
	return &hotPeerCache{
		kind:           kind,
		peersOfStore:   make(map[uint64]*TopN),
		storesOfRegion: make(map[uint64]map[uint64]struct{}),
	}
}

// RegionStats returns hot items
func (f *hotPeerCache) RegionStats() map[uint64][]*HotPeerStat {
	res := make(map[uint64][]*HotPeerStat)
	for storeID, peers := range f.peersOfStore {
		values := peers.GetAll()
		stat := make([]*HotPeerStat, len(values))
		res[storeID] = stat
		for i := range values {
			stat[i] = values[i].(*HotPeerStat)
		}
	}
	return res
}

// Update updates the items in statistics.
func (f *hotPeerCache) Update(item *HotPeerStat) {
	if item.IsNeedDelete() {
		if peers, ok := f.peersOfStore[item.StoreID]; ok {
			peers.Remove(item.RegionID)
		}

		if stores, ok := f.storesOfRegion[item.RegionID]; ok {
			delete(stores, item.StoreID)
		}
	} else {
		peers, ok := f.peersOfStore[item.StoreID]
		if !ok {
			peers = NewTopN(dimLen, topNN, topNTTL)
			f.peersOfStore[item.StoreID] = peers
		}
		peers.Put(item)

		stores, ok := f.storesOfRegion[item.RegionID]
		if !ok {
			stores = make(map[uint64]struct{})
			f.storesOfRegion[item.RegionID] = stores
		}
		stores[item.StoreID] = struct{}{}
	}
}

// CheckRegionFlow checks the flow information of region.
func (f *hotPeerCache) CheckRegionFlow(region *core.RegionInfo, storesStats *StoresStats) (ret []*HotPeerStat) {
	totalBytes := float64(f.getTotalBytes(region))
	totalKeys := float64(f.getTotalKeys(region))
	totalOps := float64(f.getTotalOps(region))

	totalOtherBytes := float64(f.getTotalOtherBytes(region))
	totalOtherKeys := float64(f.getTotalOtherKeys(region))
	totalOtherOps := float64(f.getTotalOtherOps(region))

	reportInterval := region.GetInterval()
	interval := reportInterval.GetEndTimestamp() - reportInterval.GetStartTimestamp()

	byteRate := totalBytes / float64(interval)
	keyRate := totalKeys / float64(interval)
	ops := totalOps / float64(interval)

	otherByteRate := totalOtherBytes / float64(interval)
	otherKeyRate := totalOtherKeys / float64(interval)
	otherOps := totalOtherOps / float64(interval)

	// old region is in the front and new region is in the back
	// which ensures it will hit the cache if moving peer or transfer leader occurs with the same replica number

	var tmpItem *HotPeerStat
	storeIDs := f.getAllStoreIDs(region)
	for _, storeID := range storeIDs {
		isExpired := f.isRegionExpired(region, storeID) // transfer leader or remove peer
		oldItem := f.getOldHotPeerStat(region.GetID(), storeID)
		if isExpired && oldItem != nil {
			tmpItem = oldItem
		}

		// This is used for the simulator. Ignore if report too fast.
		if !isExpired && Denoising && interval < hotRegionReportMinInterval {
			continue
		}

		newItem := &HotPeerStat{
			StoreID:        storeID,
			RegionID:       region.GetID(),
			Kind:           f.kind,
			ByteRate:       byteRate,
			KeyRate:        keyRate,
			Ops:            ops,
			OtherByteRate:  otherByteRate,
			OtherKeyRate:   otherKeyRate,
			OtherOps:       otherOps,
			LastUpdateTime: time.Now(),
			Version:        region.GetMeta().GetRegionEpoch().GetVersion(),
			needDelete:     isExpired,
			isLeader:       region.GetLeader().GetStoreId() == storeID,
		}

		if oldItem == nil {
			if tmpItem != nil { // use the tmpItem cached from the store where this region was in before
				oldItem = tmpItem
			} else { // new item is new peer after adding replica
				for _, storeID := range storeIDs {
					oldItem = f.getOldHotPeerStat(region.GetID(), storeID)
					if oldItem != nil {
						break
					}
				}
			}
		}

		newItem = f.updateHotPeerStat(newItem, oldItem, storesStats)
		if newItem != nil {
			ret = append(ret, newItem)
		}
	}

	return ret
}

func (f *hotPeerCache) IsRegionHot(region *core.RegionInfo, hotDegree int) bool {
	switch f.kind {
	case WriteFlow:
		return f.isRegionHotWithAnyPeers(region, hotDegree)
	case ReadFlow:
		return f.isRegionHotWithPeer(region, region.GetLeader(), hotDegree)
	}
	return false
}

func (f *hotPeerCache) CollectMetrics(typ string) {
	for storeID, peers := range f.peersOfStore {
		store := storeTag(storeID)
		thresholds := f.calcHotThresholds(storeID)
		hotCacheStatusGauge.WithLabelValues("total_length", store, typ).Set(float64(peers.Len()))
		hotCacheStatusGauge.WithLabelValues("byte-rate-threshold", store, typ).Set(thresholds[byteDim])
		hotCacheStatusGauge.WithLabelValues("key-rate-threshold", store, typ).Set(thresholds[keyDim])
		// for compatibility
		hotCacheStatusGauge.WithLabelValues("hotThreshold", store, typ).Set(thresholds[byteDim])
	}
}

func (f *hotPeerCache) getTotalBytes(region *core.RegionInfo) uint64 {
	switch f.kind {
	case WriteFlow:
		return region.GetBytesWritten()
	case ReadFlow:
		return region.GetBytesRead()
	}
	return 0
}

func (f *hotPeerCache) getTotalKeys(region *core.RegionInfo) uint64 {
	switch f.kind {
	case WriteFlow:
		return region.GetKeysWritten()
	case ReadFlow:
		return region.GetKeysRead()
	}
	return 0
}

func (f *hotPeerCache) getTotalOps(region *core.RegionInfo) uint64 {
	switch f.kind {
	case WriteFlow:
		return region.GetOpsWrite()
	case ReadFlow:
		return region.GetOpsRead()
	}
	return 0
}

func (f *hotPeerCache) getTotalOtherBytes(region *core.RegionInfo) uint64 {
	switch f.kind {
	case ReadFlow:
		return region.GetBytesWritten()
	case WriteFlow:
		return region.GetBytesRead()
	}
	return 0
}

func (f *hotPeerCache) getTotalOtherKeys(region *core.RegionInfo) uint64 {
	switch f.kind {
	case ReadFlow:
		return region.GetKeysWritten()
	case WriteFlow:
		return region.GetKeysRead()
	}
	return 0
}

func (f *hotPeerCache) getTotalOtherOps(region *core.RegionInfo) uint64 {
	switch f.kind {
	case ReadFlow:
		return region.GetOpsWrite()
	case WriteFlow:
		return region.GetOpsRead()
	}
	return 0
}

func (f *hotPeerCache) getOldHotPeerStat(regionID, storeID uint64) *HotPeerStat {
	if hotPeers, ok := f.peersOfStore[storeID]; ok {
		if v := hotPeers.Get(regionID); v != nil {
			return v.(*HotPeerStat)
		}
	}
	return nil
}

func (f *hotPeerCache) isRegionExpired(region *core.RegionInfo, storeID uint64) bool {
	switch f.kind {
	case WriteFlow:
		return region.GetStorePeer(storeID) == nil
	case ReadFlow:
		return region.GetLeader().GetStoreId() != storeID
	}
	return false
}

func (f *hotPeerCache) calcHotThresholds(storeID uint64) [dimLen]float64 {
	minThresholds := minHotThresholds[f.kind]
	tn, ok := f.peersOfStore[storeID]
	if !ok || tn.Len() < topNN {
		return minThresholds
	}
	ret := [dimLen]float64{
		byteDim:      tn.GetTopNMin(byteDim).(*HotPeerStat).ByteRate,
		keyDim:       tn.GetTopNMin(keyDim).(*HotPeerStat).KeyRate,
		opsDim:       tn.GetTopNMin(opsDim).(*HotPeerStat).Ops,
		otherByteDim: tn.GetTopNMin(otherByteDim).(*HotPeerStat).OtherByteRate,
		otherKeyDim:  tn.GetTopNMin(otherKeyDim).(*HotPeerStat).OtherKeyRate,
		otherOpsDim:  tn.GetTopNMin(otherOpsDim).(*HotPeerStat).OtherOps,
	}
	for k := 0; k < dimLen; k++ {
		// ret[k] = math.Max(ret[k]*hotThresholdRatio, minThresholds[k])
		ret[k] = minThresholds[k]
	}
	return ret
}

func (f *hotPeerCache) ReduceHotThresholds() {
	minThresholds := minHotThresholds[f.kind]
	for k := 0; k < dimLen; k++ {
		minThresholds[k] /= 2
	}
}

// gets the storeIDs, including old region and new region
func (f *hotPeerCache) getAllStoreIDs(region *core.RegionInfo) []uint64 {
	storeIDs := make(map[uint64]struct{})
	ret := make([]uint64, 0, len(region.GetPeers()))
	// old stores
	ids, ok := f.storesOfRegion[region.GetID()]
	if ok {
		for storeID := range ids {
			storeIDs[storeID] = struct{}{}
			ret = append(ret, storeID)
		}
	}

	// new stores
	for _, peer := range region.GetPeers() {
		// ReadFlow no need consider the followers.
		if f.kind == ReadFlow && peer.GetStoreId() != region.GetLeader().GetStoreId() {
			continue
		}
		if _, ok := storeIDs[peer.GetStoreId()]; !ok {
			storeIDs[peer.GetStoreId()] = struct{}{}
			ret = append(ret, peer.GetStoreId())
		}
	}

	return ret
}

func (f *hotPeerCache) isRegionHotWithAnyPeers(region *core.RegionInfo, hotDegree int) bool {
	for _, peer := range region.GetPeers() {
		if f.isRegionHotWithPeer(region, peer, hotDegree) {
			return true
		}
	}
	return false
}

func (f *hotPeerCache) isRegionHotWithPeer(region *core.RegionInfo, peer *metapb.Peer, hotDegree int) bool {
	if peer == nil {
		return false
	}
	storeID := peer.GetStoreId()
	if peers, ok := f.peersOfStore[storeID]; ok {
		if stat := peers.Get(region.GetID()); stat != nil {
			return stat.(*HotPeerStat).HotDegree >= hotDegree
		}
	}
	return false
}

func (f *hotPeerCache) updateHotPeerStat(newItem, oldItem *HotPeerStat, storesStats *StoresStats) *HotPeerStat {
	thresholds := f.calcHotThresholds(newItem.StoreID)
	isHot := newItem.ByteRate >= thresholds[byteDim] ||
		newItem.KeyRate >= thresholds[keyDim] ||
		newItem.Ops >= thresholds[opsDim]

	if updateWithOtherStats {
		isHot = isHot || newItem.OtherByteRate >= thresholds[otherByteDim] ||
			newItem.OtherKeyRate >= thresholds[otherKeyDim] ||
			newItem.OtherOps >= thresholds[otherOpsDim]
	}

	if newItem.needDelete {
		return newItem
	}

	if oldItem != nil {
		newItem.rollingByteRate = oldItem.rollingByteRate
		newItem.rollingKeyRate = oldItem.rollingKeyRate
		newItem.rollingOps = oldItem.rollingOps
		newItem.rollingOtherByteRate = oldItem.rollingOtherByteRate
		newItem.rollingOtherKeyRate = oldItem.rollingOtherKeyRate
		newItem.rollingOtherOps = oldItem.rollingOtherOps
		if isHot {
			newItem.HotDegree = oldItem.HotDegree + 1
			newItem.AntiCount = hotRegionAntiCount
		} else {
			newItem.HotDegree = oldItem.HotDegree - 1
			newItem.AntiCount = oldItem.AntiCount - 1
			if newItem.AntiCount <= 0 {
				newItem.needDelete = true
			}
		}
	} else {
		if !isHot {
			return nil
		}
		newItem.rollingByteRate = NewMedianFilter(rollingWindowsSize)
		newItem.rollingKeyRate = NewMedianFilter(rollingWindowsSize)
		newItem.rollingOps = NewMedianFilter(rollingWindowsSize)
		newItem.rollingOtherByteRate = NewMedianFilter(rollingWindowsSize)
		newItem.rollingOtherKeyRate = NewMedianFilter(rollingWindowsSize)
		newItem.rollingOtherOps = NewMedianFilter(rollingWindowsSize)
		newItem.AntiCount = hotRegionAntiCount
		newItem.isNew = true
	}

	newItem.rollingByteRate.Add(newItem.ByteRate)
	newItem.rollingKeyRate.Add(newItem.KeyRate)
	newItem.rollingOps.Add(newItem.Ops)
	newItem.rollingOtherByteRate.Add(newItem.OtherByteRate)
	newItem.rollingOtherKeyRate.Add(newItem.OtherKeyRate)
	newItem.rollingOtherOps.Add(newItem.OtherOps)

	return newItem
}
