import numpy as np
import pulp
import random
import time
import logging
import sys
import math

logger = logging.getLogger("solver")
logger.setLevel(logging.DEBUG)

ch = logging.StreamHandler()
ch.setLevel(logging.DEBUG)
formatter = logging.Formatter('%(asctime)s - %(levelname)s - %(message)s')
ch.setFormatter(formatter)

logger.addHandler(ch)

class RegionInfo(object):
    def __init__(self, id, loads):
        self.id = id
        self.vals = loads
        self.src_sid = 0
        self.parent_id = 0
    @property
    def bytes(self):
        return self.vals[0]
    @bytes.setter
    def bytes(self, v):
        self.vals[0] = v
    @property
    def keys(self):
        return self.vals[1]
    @keys.setter
    def keys(self, v):
        self.vals[1] = v

class DomRegions(object):
    def __init__(self):
        self.regions = [{}, {}]
        self.migrated_regions = {}
        self.count = [0, 0]
        self.id = 100 * 1000000

    def allocateId(self):
        self.id += 1
        return str(self.id)

    def push(self, which, region):
        if region.src_sid not in self.regions[which]:
            self.regions[which][region.src_sid] = []
        self.regions[which][region.src_sid].append(region)

        self.migrated_regions[region.id] = region
        self.count[which] += 1
    
    def empty(self, which):
        return self.count[which] == 0

    def pop(self, which, candi_sid, ratio = None, bases = None):
        sid = candi_sid
        if sid not in self.regions[which]:  # pick from remote node
            for sid_ in self.regions[which]:
                sid = sid_
                break
        region = self.regions[which][sid].pop()
        region.dst_sid = candi_sid

        if ratio == None or region.vals[which] / bases[which] <= ratio:
            if len(self.regions[which][sid]) == 0:
                del self.regions[which][sid]
                
            self.count[which] -= 1
            return region
        else:
            new_region = self.splitRegion(region, which, ratio, bases)
            self.regions[which][sid].append(region)     # may need to plug into another dom regions
            return new_region

    def splitRegion(self, region, which, ratio, bases):
        splitted_ratio = bases[which] * ratio / region.vals[which]
        splitted_vals = [region.vals[i] * splitted_ratio for i in range(2)]
        new_region = RegionInfo(self.allocateId(), [splitted_vals[0], splitted_vals[1]])
        new_region.src_sid = region.src_sid
        new_region.dst_sid = region.dst_sid
        new_region.parent_id = region.id

        region.vals[0] -= splitted_vals[0]
        region.vals[1] -= splitted_vals[1]
        assert region.vals[0] >= 0 and region.vals[1] >= 0
        logger.debug("in store %s, region from store %s, split into [id %s, ratio %.2f, %.2f] + [id %s, ratio %.2f, %.2f]" % (region.dst_sid, region.src_sid, region.id, region.vals[0] / bases[0], region.vals[1] / bases[1], new_region.id, new_region.vals[0] / bases[0], new_region.vals[1] / bases[1]))
        return new_region

    def splitRegionWithVal(self, region, higher, ratio, bases, diff, calculate = False):
        val_upper = region.vals[higher] / bases[higher]
        val_lower = region.vals[1 - higher] / bases[1 - higher]
        x = diff / (val_upper - val_lower)
        splitted_vals = [region.vals[i] * x for i in range(2)]
        if calculate:
            return [splitted_vals[i] / bases[i] for i in range(2)]
        
        new_region = RegionInfo(self.allocateId(), [splitted_vals[0], splitted_vals[1]])
        new_region.src_sid = region.src_sid
        new_region.dst_sid = new_region.src_sid
        new_region.parent_id = region.id

        region.vals[0] -= splitted_vals[0]
        region.vals[1] -= splitted_vals[1]
        assert region.vals[0] >= 0 and region.vals[1] >= 0
        logger.debug("in store %s, split to equal, region from store %s, split into [id %s, ratio %.2f, %.2f] + [id %s, ratio %.2f, %.2f]" % (region.src_sid, region.src_sid, region.id, region.vals[0] / bases[0], region.vals[1] / bases[1], new_region.id, new_region.vals[0] / bases[0], new_region.vals[1] / bases[1]))
        return new_region

    def getMigratedRegions(self):
        ret = []
        for rid in self.migrated_regions:
            region = self.migrated_regions[rid]
            if region.src_sid != region.dst_sid:
                ret.append(region)
        return ret
    
    def buildSolution(self):
        ret = []
        regions = self.getMigratedRegions()
        for region in regions:
            ret.append([str(region.id), region.src_sid, region.dst_sid])
        return ret

class StoredRegions(object):
    def __init__(self, store_info, dim_id):
        self.dim_id = dim_id
        self.sorted_regions = sorted(store_info.regions.values(), key = lambda r : r.vals[dim_id])
        self.remain_loads = 0
        for r in self.sorted_regions:
            self.remain_loads += r.vals[dim_id]
    
    def pop():
        r = self.sorted_regions.pop()
        self.remain_loads -= r.vals[dim_id]
        return r

class StoreInfo(object):
    max_rid = 1
    def __init__(self, load_nums, id):
        self.regions = {}
        self.is_sorted = False
        self.sorted_regions = None
        self.id = id
        self.vals_num = load_nums
        self.vals_sum = [0] * self.vals_num
        self.act_vals_sum = [0] * self.vals_num
        self.dom_regions = None

    def add(self, region, check = True):
        if check and region.id in self.regions:
            print("region ", region.id, " already exists in store ", self.id)
            raise "Existing region"
        self.regions[region.id] = region
        for i in range(self.vals_num):
            self.vals_sum[i] += region.vals[i]
            self.act_vals_sum[i] += region.vals[i]
    
    def remove(self, region):
        del self.regions[region.id]
        for i in range(self.vals_num):
            self.vals_sum[i] -= region.vals[i]
            self.act_vals_sum[i] -= region.vals[i]
    
    def splitRegion(self, region, num):
        self.remove(region)
        print("in store %s, split region %d into %d pieces" % (self.id, region.id, num))
        for _ in range(num):
            new_id = self.allocateId()
            r = RegionInfo(new_id, [region.bytes / num, region.keys / num])
            self.add(r)
    
    def getRandomRegion(self):
        return random.choice(list(self.regions.values()))

    def sort(self, which, reverse = False):
        s = sorted(self.regions.values(), key = lambda r : r.vals[which])
        # self.sorted_regions = collections.deque(s)
        self.sorted_regions = s
        if reverse:
            self.sorted_regions.reverse()
        self.is_sorted = True
    
    def sortAll(self):
        self.sorted_regions = []
        for which in range(self.vals_num):
            self.sorted_regions.append(sorted(self.regions.values(), key = lambda r : r.vals[which]))
    
    def classifyRegions(self, bases):
        self.dom_regions = [[], []]
        for region in self.regions.values():
            if math.isclose(region.vals[0] / bases[0], region.vals[1] / bases[1], rel_tol=1e-5):
                self.dom_regions[0].append(region)
                self.dom_regions[1].append(region)
            elif region.vals[0] / bases[0] > region.vals[1] / bases[1]:
                self.dom_regions[0].append(region)
            else:
                self.dom_regions[1].append(region)
                
        for i in range(2): #self.vals_num
            self.dom_regions[i].sort(key=lambda r: abs(r.vals[0] - r.vals[1])) #, reverse=True)
    
    def sortRegionByMaxLoad(self):
        self.sorted_regions = sorted(self.regions.values(), key = lambda r : np.max(r.vals))
        self.is_sorted = True

    def ifMoveIn(self, region, which):
        return self.act_vals_sum[which] + region.vals[which]
    
    def ifMoveOut(self, region, which):
        return self.act_vals_sum[which] - region.vals[which]
    
    @staticmethod
    def migrate(region, from_store, to_store):
        from_store.remove(region)
        to_store.add(region)
    
    @staticmethod
    def calcCV(store_infos, which):
        vals = [store.vals_sum[which] for store in store_infos]
        cv = np.std(vals) / np.mean(vals)
        return cv
    
    @classmethod
    def allocateId(cls):
        cls.max_rid += 1
        return cls.max_rid
    
    @classmethod
    def shuffle(cls, store_infos, migrate_nums):
        regions = []
        for store in store_infos:
            regions.extend(store.regions.values())
            store.regions = {}
            store.vals_sum = [0] * store.vals_num
            store.act_vals_sum = [0] * store.vals_num
        store_nums = len(store_infos)
        for region in regions:
            src_sid = random.randint(0, store_nums - 1)
            store_infos[src_sid].add(region)

        # rebuild store flow 
        # for store in store_infos:
        #     regions = []
        #     regions.extend(store.regions.values())
        #     store.regions = {}
        #     store.vals_sum = [0] * store.vals_num
        #     store.act_vals_sum = [0] * store.vals_num
        #     store_nums = len(store_infos)
        #     for region in regions:
        #         store.add(region)

        # store_nums = len(store_infos)
        # for i in range(migrate_nums):
        #     src_sid = random.randint(0, store_nums - 1)
        #     dst_sid = random.randint(0, store_nums - 1)
        #     while dst_sid == src_sid:
        #         dst_sid = random.randint(0, store_nums - 1)

        #     src_store = store_infos[src_sid]
        #     dst_store = store_infos[dst_sid]

        #     if len(src_store.regions) == 0:
        #         continue
        #     region = src_store.getRandomRegion()
        #     # while region.id in moved_rid:
        #     #     region = src_store.getRandomRegion()
        #     # moved_rid[region.id] = True

        #     StoreInfo.migrate(region, src_store, dst_store)
        #     region.src_sid = dst_store.id

    @classmethod
    def balanceSingle(cls, store_infos, ratio, which, enable_splitting = True):
        ret = []

        # sort stores and regions by specified attribute
        store_infos.sort(key = lambda s: s.act_vals_sum[which])
        for store in store_infos:
            store.sort(which)
            if enable_splitting:
                num = len(store.regions) > 10 and 10 or len(store.regions)
                top10_vals = [r.vals[which] for r in store.sorted_regions[-num - 1:]]
                median = np.median(top10_vals)
                for r in reversed(store.sorted_regions):
                    if r.vals[which] >= 2 * median:
                        store.splitRegion(r, int(r.vals[which] / median * 2))
                store.sort(which)
        
        # calc expected value
        vals = [store.act_vals_sum[which] for store in store_infos]
        val_expected = np.mean(vals)
        val_upper = val_expected * (1 + ratio)
        val_lower = val_expected * (1 - ratio)
        state = {-2 : "less than lower bound", -1 : "less than expect", 1 : "greater than expect", 2 : "greater than upper bound"}
        def checkValue(val):
            if val > val_upper:
                return 2
            elif val > val_expected:
                return 1
            elif val > val_lower:
                return -1
            else:
                return -2
        
        low = 0
        high = len(store_infos) - 1
        while low < high:
            hstore = store_infos[high]
            val = hstore.act_vals_sum[which]            
            if abs(checkValue(val)) <= 1:
                high -= 1
                continue
            if checkValue(val) == -2:   # too lower store
                break
                
            i = len(hstore.sorted_regions) - 1  # from hottest region
            while i >= 0 and checkValue(hstore.act_vals_sum[which]) == 2:
                cur_region = hstore.sorted_regions[i]
                if checkValue(hstore.ifMoveOut(cur_region, which)) == -2:   # too hot
                    i -= 1
                    continue
                
                for store_i in range(low, high):
                    lstore = store_infos[store_i]
                    if checkValue(lstore.ifMoveIn(cur_region, which)) != 2:  # can hold this hot region
                        StoreInfo.migrate(cur_region, hstore, lstore)
                        # print("move region %d (takes %.2f%%) from store %d to store %d" % (cur_region.id, 100.0 * cur_region.vals[which] / val_expected, high, store_i))
                        ret.append([str(cur_region.id), hstore.id, lstore.id])
                        break
                i -= 1
            
            if checkValue(hstore.act_vals_sum[which]) == 2: # all of the regions are too hot to find proper dest store
                break
            
            high -= 1

        if low != high: # failed to balance the load
            return ret, False
        else:
            return ret, True
    
    @classmethod
    def balanceMultiGreedy(cls, store_infos, ratio):
        load_nums = store_infos[0].vals_num
        # calc expected loads of stores
        val_expected = []
        for which in range(load_nums):
            vals = [store.act_vals_sum[which] for store in store_infos]
            val_expected.append(np.mean(vals))

        # ratio = 0.1

        #normalize load
        for store in store_infos:
            store.vals_sum = np.true_divide(store.vals_sum, val_expected)
            store.act_vals_sum = np.true_divide(store.act_vals_sum, val_expected)
            logger.debug("store id %s vals: %s" % (store.id, str(store.act_vals_sum)))
            for region in store.regions.values():
                region.vals = np.true_divide(region.vals, val_expected)
                region.has_moved = False
                if np.max(region.vals) > ratio:
                    logger.debug("find high load region %s: %s" % (region.id, region.vals))

        def maxFlowID(store):
            id = 0
            flow = 0
            for i in range(load_nums):
                if store.act_vals_sum[i] > flow:
                    flow = store.act_vals_sum[i]
                    id = i
            return id, flow

        def pickBestDstStore(which, region):
            if not hasattr(region, 'peer_stores'):
                region.peer_stores = [region.src_sid]
            logger.debug("region peer store %s" % str(region.peer_stores))
            dst_store = None
            min_load = 10000
            for store in store_infos:
                if store.id in region.peer_stores:
                    continue
                after_load = store.act_vals_sum[which] + region.vals[which]
                if after_load <= 1 + ratio:
                    if after_load < min_load:
                        dst_store = store
                        min_load = after_load
            return dst_store

        store_infos.sort(key = lambda s: s.act_vals_sum[0], reverse = True)

        ret = []
        has_run = True
        while has_run:
            has_run = False
            for i in range(len(store_infos)):
                cur_store = store_infos[i]

                cur_store.classifyRegions([1]*load_nums)
                while True:
                    max_id, max_flow = maxFlowID(cur_store)
                    logger.debug("store %s: current state -- max_id %d, flow %s, regionNum %d, domregion num %d %d" % (cur_store.id, max_id, 
                        str(cur_store.act_vals_sum), len(cur_store.regions), 
                        len(cur_store.dom_regions[0]), len(cur_store.dom_regions[1])))
                    if max_flow > 1 + ratio:
                        while True:
                            if len(cur_store.dom_regions[max_id]) == 0:
                                logger.debug("no available region")
                                # o_id = 1 - max_id
                                # sum_vals = [0] * load_nums
                                # for re in cur_store.dom_regions[o_id]:
                                #     logger.debug("store %s, odom region flow %s" % (cur_store.id, str(re.vals)))
                                #     sum_vals = np.add(sum_vals, re.vals)

                                region = None
                                break
                            region = cur_store.dom_regions[max_id].pop()
                            break
                            if not region.has_moved:
                                break
                            else:
                                logger.debug("region has moved")
                        if region == None:
                            logger.debug("store %s: can't optimize flow %s" % (cur_store.id, str(cur_store.act_vals_sum)))
                            for store in store_infos:
                                logger.debug("global stats: store id %s, flow %s" % (store.id, str(store.act_vals_sum)))
                            break
                        
                        if max_flow - region.vals[max_id] >= 1 - ratio:
                            logger.debug("store %s: consider region %s" % (cur_store.id, str(region.vals)))
                            dst_store = pickBestDstStore(max_id, region)
                            if dst_store == None:
                                logger.debug("not found suitable stores")
                                continue
                            cur_store.remove(region)
                            dst_store.add(region)
                            ret.append([str(region.id), cur_store.id, dst_store.id])
                            region.has_moved = True
                            logger.debug("store %s: find placement, move region %s, to store %s flow %s" % (cur_store.id, str(region.vals), dst_store.id, str(dst_store.act_vals_sum)))
                            has_run = True
                        else:
                            logger.debug("store %s: region %s will destroy limitation" % (cur_store.id, str(region.vals)))
                    else:
                        break
        return ret
    
    @classmethod
    def balanceMultiGreedyGeneral(cls, store_infos, ratio):
        load_nums = store_infos[0].vals_num
        # calc expected loads of stores
        val_expected = []
        for which in range(load_nums):
            vals = [store.act_vals_sum[which] for store in store_infos]
            val_expected.append(np.mean(vals))

        # ratio = 0.1

        #normalize load
        for store in store_infos:
            store.vals_sum = np.true_divide(store.vals_sum, val_expected)
            store.act_vals_sum = np.true_divide(store.act_vals_sum, val_expected)
            logger.debug("store id %s vals: %s" % (store.id, str(store.act_vals_sum)))
            for region in store.regions.values():
                assert region.id in store.regions
                region.vals = np.true_divide(region.vals, val_expected)
                region.has_moved = False
                if np.max(region.vals) > ratio:
                    logger.debug("find high load region %s: %s" % (region.id, region.vals))

        def maxFlowID(store):
            id = 0
            flow = 0
            for i in range(load_nums):
                if store.act_vals_sum[i] > flow:
                    flow = store.act_vals_sum[i]
                    id = i
            return id, flow

        def pickBestDstStore(srcStore, which, region):
            if not hasattr(region, 'peer_stores'):
                region.peer_stores = [region.src_sid]
            logger.debug("region peer store %s" % str(region.peer_stores))
            min_load = 10000
            ret_store = None
            for store in store_infos:
                if store.id in region.peer_stores:
                    # logger.debug("pickBestDstStore: in peer stores")
                    continue
                if store.act_vals_sum[which] > srcStore.act_vals_sum[which]:
                    continue
                new_loads = np.add(store.act_vals_sum, region.vals)
                max_load = np.max(new_loads)
                # if max_load <= 1 + ratio:
                if max_load < min_load:
                    min_load = max_load
                    ret_store = store
            return ret_store

        store_infos.sort(key = lambda s: s.act_vals_sum[0], reverse = True)

        ret = []
        has_run = True
        while has_run:
            has_run = False
            for i in range(len(store_infos)):
                cur_store = store_infos[i]
                cur_store.sortAll()

                while True:
                    max_id, max_flow = maxFlowID(cur_store)
                    logger.debug("store %s: current states -- max_id %d, flow %s, regionNum %d" % (cur_store.id, max_id, str(cur_store.act_vals_sum), len(cur_store.regions)))
                    if max_flow > 1 + ratio:
                        while True:
                            if len(cur_store.sorted_regions[max_id]) == 0:
                                logger.debug("no available region")
                                region = None
                                break
                            region = cur_store.sorted_regions[max_id].pop()
                            if not region.has_moved:
                                break
                            else:
                                logger.debug("region has moved")
                        if region == None:
                            logger.debug("store %s: can't optimize flow %s" % (cur_store.id, str(cur_store.act_vals_sum)))
                            for store in store_infos:
                                logger.debug("global stats: store id %s, flow %s" % (store.id, str(store.act_vals_sum)))
                            break
                        
                        pre_diff = np.max(cur_store.act_vals_sum) - np.min(cur_store.act_vals_sum)
                        new_loads = np.subtract(cur_store.act_vals_sum, region.vals)
                        after_diff = np.max(new_loads) - np.min(new_loads)

                        diff_condi = True
                        if (np.max(cur_store.act_vals_sum) - 1) * (np.min(cur_store.act_vals_sum) - 1) < 0 and pre_diff < after_diff:
                            diff_condi = False

                        if max_flow - region.vals[max_id] >= 1 - ratio: # and diff_condi
                        # if True:
                        # if np.min(new_loads) >= 1 - ratio:
                            logger.debug("store %s: consider region %s" % (cur_store.id, str(region.vals)))
                            dst_store = pickBestDstStore(cur_store, max_id, region)
                            if dst_store == None:
                                logger.debug("not found suitable stores")
                                continue
                            cur_store.remove(region)
                            dst_store.add(region)
                            ret.append([str(region.id), cur_store.id, dst_store.id])
                            region.has_moved = True
                            logger.debug("store %s: find placement, move region %s, to store %s flow %s" % (cur_store.id, str(region.vals), dst_store.id, str(dst_store.act_vals_sum)))
                            has_run = True
                        else:
                            logger.debug("store %s: region %s will destroy limitation, pre_diff %f after_diff %f" % (cur_store.id, str(region.vals), pre_diff, after_diff))
                    else:
                        break
        return ret
                
    # assume the load of each region is less than ratio.
    @classmethod
    def greedy(cls, store_infos, ratio):
        load_nums = 2
        # calc expected loads of stores
        val_expected = []
        for which in range(load_nums):
            vals = [store.act_vals_sum[which] for store in store_infos]
            val_expected.append(np.mean(vals))

        def flowRatio(store, which):
            return store.act_vals_sum[which] / val_expected[which]

        def whichLoadType(store):
            if flowRatio(store, 0) > 1 and flowRatio(store, 1) > 1:
                return "above" # both overflow
            elif (flowRatio(store, 0) - 1) * (flowRatio(store, 1) - 1) < 0:
                return "cross" # one overflow, another not full
            else:
                return "under" # both not full
            
        def pickHigher(store):
            which = 0
            if flowRatio(store, 0) < flowRatio(store, 1):
                which = 1
            region = store.dom_regions[which].pop()
            return (which, region)
            
        dom_regions = DomRegions()
            
        # step 1: balance loads of two dimensions
        for store in store_infos:
            store.classifyRegions(val_expected)
            while abs(flowRatio(store, 0) - flowRatio(store, 1)) > ratio or whichLoadType(store) == "above":
                which, region = pickHigher(store)
                store.remove(region)
                dom_regions.push(which, region)  # remove overflowed region to global dom_regions
            logger.debug("step1: balance loads of two dimensions, store id %s, flow ratio (%.2f, %.2f)" % (store.id, flowRatio(store, 0), flowRatio(store, 1)))
        
        # # original version
        # store_infos.sort(key=lambda s: s.act_vals_sum[0])
        # for store in store_infos:
        #     while whichLoadType(store) != "above":
        #         which = 0
        #         if flowRatio(store, 0) > flowRatio(store, 1):
        #             which = 1
            
        #         if len(dom_regions[which]) != 0:    # pick region from global dom_regions
        #             region = dom_regions[which].pop() # prior to choose original region
        #             store.add(region)
        #         else: # no region can to be migrated
        #             break
        #     logger.debug("fill store, id %s: (%.2f, %.2f)" % (store.id, flowRatio(store, 0), flowRatio(store, 1)))
        # for which in range(load_nums):
        #     store_infos.sort(key=lambda s: s.act_vals_sum[which])
        #     for store in store_infos:
        #         while flowRatio(store, which) < 1 and len(dom_regions[which]) != 0:
        #             region = dom_regions[which].pop()
        #             store.add(region)
        # assert len(dom_regions[0]) == 0 and len(dom_regions[1]) == 0
        # return

        # step 2: balance not full stores
        candi_stores = [store for store in store_infos if whichLoadType(store) == "under"]
        candi_stores.sort(key=lambda s: s.act_vals_sum[0])
        for store in candi_stores:
            while whichLoadType(store) == "under":
                which = 0
                if flowRatio(store, 0) > flowRatio(store, 1):
                    which = 1
            
                if not dom_regions.empty(which):    # pick region from global dom_regions
                    region = dom_regions.pop(which, store.id) # prior to choose original region
                    store.add(region)
                else: # no region can to be migrated
                    logger.debug("type %d empty" % which)
                    break
            logger.debug("step2: balance not full store, id %s: (%.2f, %.2f)" % (store.id, flowRatio(store, 0), flowRatio(store, 1)))
        
        # step 3: place remaining dom_regions
        for which in range(load_nums):
            store_infos.sort(key=lambda s: s.act_vals_sum[which])
            for store in store_infos:
                if flowRatio(store, which) > flowRatio(store, 1 - which):
                    continue
                logger.debug("before place remaining dom_regions, store id %s: (%.2f, %.2f)" % (store.id, flowRatio(store, 0), flowRatio(store, 1)))
                while whichLoadType(store) != "above":                
                    if not dom_regions.empty(which):    # pick region from global dom_regions
                        region = dom_regions.pop(which, store.id) # prior to choose original region
                        store.add(region)
                        # try to reduce upper bound
                        if flowRatio(store, 0) > 1 + ratio or flowRatio(store, 1) > 1 + ratio:
                            dom_regions.push(which, region)
                            store.remove(region)
                            break
                    else: # no region can to be migrated
                        logger.debug("type %d empty" % which)
                        break
                logger.debug("step3: place remaining dom_regions, store id %s: (%.2f, %.2f)" % (store.id, flowRatio(store, 0), flowRatio(store, 1)))

        # step 4: final placing
        for which in range(load_nums):
            store_infos.sort(key=lambda s: s.act_vals_sum[which])
            for store in store_infos:
                while flowRatio(store, which) < 1 and not dom_regions.empty(which):
                    region = dom_regions.pop(which, store.id)
                    store.add(region)

        # assert dom_regions.empty(0) and dom_regions.empty(1)
        if not (dom_regions.empty(0) and dom_regions.empty(1)):
            logger.error("remains dom regions!!! %s" % str((dom_regions.empty(0), dom_regions.empty(1))))
            for store in store_infos:
                logger.error("store %s, %.2f, %.2f" % (str(store.id), flowRatio(store, 0), flowRatio(store, 1)))
        return dom_regions.buildSolution()

    # general method
    @classmethod
    def greedySplit(cls, store_infos, ratio):
        load_nums = 2
        # calc expected loads of stores
        val_expected = []
        for which in range(load_nums):
            vals = [store.act_vals_sum[which] for store in store_infos]
            val_expected.append(np.mean(vals))

        def flowRatio(store, which):
            return store.act_vals_sum[which] / val_expected[which]

        def whichLoadType(store):
            if flowRatio(store, 0) > 1 and flowRatio(store, 1) > 1:
                return "above" # both overflow
            elif (flowRatio(store, 0) - 1) * (flowRatio(store, 1) - 1) < 0:
                return "cross" # one overflow, another not full
            else:
                return "under" # both not full
            
        def pickHigher(store):
            which = 0
            if flowRatio(store, 0) < flowRatio(store, 1):
                which = 1
            region = store.dom_regions[which].pop()
            return (which, region)
        
        def checkOrder(store):
            higher = 0
            if flowRatio(store, 0) < flowRatio(store, 1):
                higher = 1
            lower = 1 - higher
            return higher, lower

        dom_regions = DomRegions()
            
        # step 1: balance loads of two dimensions
        for store in store_infos:
            store.classifyRegions(val_expected)
            pick_count = 0
            total_count = len(store.regions)
            while abs(flowRatio(store, 0) - flowRatio(store, 1)) > ratio or whichLoadType(store) != "under":
                pre_higher, _ = checkOrder(store)
                which, region = pickHigher(store)
                store.remove(region)
                cur_higher, _ = checkOrder(store)
                dom_regions.push(which, region)  # remove overflowed region to global dom_regions
                pick_count += 1

                if pre_higher != cur_higher and abs(flowRatio(store, 0) - flowRatio(store, 1)) > ratio and whichLoadType(store) == "under":
                    diff = abs(flowRatio(store, 0) - flowRatio(store, 1))
                    # pretend to split region, then store's loads will have the same value
                    splitted_loads = dom_regions.splitRegionWithVal(region, pre_higher, ratio, val_expected, diff, calculate = True)
                    if flowRatio(store, 0) + splitted_loads[0] > 1 + ratio:     # current splitting will make store be overflowed, try to split next region
                        continue
                    
                    new_region = dom_regions.splitRegionWithVal(region, pre_higher, ratio, val_expected, diff)
                    store.add(new_region)

                    break
            logger.debug("store id %s, # of regions: %d, # of picked region: %d" % (store.id, total_count, pick_count))
            logger.debug("step1: balance loads of two dimensions, store id %s, flow ratio (%.2f, %.2f)" % (store.id, flowRatio(store, 0), flowRatio(store, 1)))
        
        # original version
        store_infos.sort(key=lambda s: s.act_vals_sum[0])
        for store in store_infos:
            while whichLoadType(store) != "above":
                which = 0
                if flowRatio(store, 0) > flowRatio(store, 1):
                    which = 1
            
                if not dom_regions.empty(which):    # pick region from global dom_regions
                    region = dom_regions.pop(which, store.id, ratio, val_expected) # prior to choose original region
                    store.add(region)
                else: # no region can to be migrated
                    break
            logger.debug("fill store, id %s: (%.2f, %.2f)" % (store.id, flowRatio(store, 0), flowRatio(store, 1)))
        for which in range(load_nums):     # remains some regions which have the same type
            store_infos.sort(key=lambda s: s.act_vals_sum[which])
            for store in store_infos:
                while flowRatio(store, which) < 1 and not dom_regions.empty(which):
                    region = dom_regions.pop(which, store.id, ratio, val_expected)
                    store.add(region)
        # assert dom_regions.empty(0) and dom_regions.empty(1)
        if not (dom_regions.empty(0) and dom_regions.empty(1)):
            logger.error("remains dom regions!!! %s" % str((dom_regions.empty(0), dom_regions.empty(1))))
            for store in store_infos:
                logger.error("store %s, %.2f, %.2f" % (str(store.id), flowRatio(store, 0), flowRatio(store, 1)))
            for i in range(2):
                while not dom_regions.empty(i):
                    region = dom_regions.pop(i, 0, 1, val_expected)
                    logger.error("remains region: %.2f, %.2f" % (region.vals[0] / val_expected[0], region.vals[1] / val_expected[1]))
        # end
        return dom_regions.buildSolution()  

        # ------------ maybe should consider remaining part

        # step 2: balance not full stores
        candi_stores = [store for store in store_infos if whichLoadType(store) == "under"]
        candi_stores.sort(key=lambda s: s.act_vals_sum[0])
        for store in candi_stores:
            while whichLoadType(store) == "under":
                which = 0
                if flowRatio(store, 0) > flowRatio(store, 1):
                    which = 1
            
                if not dom_regions.empty(which):    # pick region from global dom_regions
                    region = dom_regions.pop(which, store.id) # prior to choose original region
                    store.add(region)
                else: # no region can to be migrated
                    logger.debug("type %d empty" % which)
                    break
            logger.debug("step2: balance not full store, id %s: (%.2f, %.2f)" % (store.id, flowRatio(store, 0), flowRatio(store, 1)))
        
        # step 3: place remaining dom_regions
        for which in range(load_nums):
            store_infos.sort(key=lambda s: s.act_vals_sum[which])
            for store in store_infos:
                if flowRatio(store, which) > flowRatio(store, 1 - which):
                    continue
                logger.debug("before place remaining dom_regions, store id %s: (%.2f, %.2f)" % (store.id, flowRatio(store, 0), flowRatio(store, 1)))
                while whichLoadType(store) != "above":                
                    if not dom_regions.empty(which):    # pick region from global dom_regions
                        region = dom_regions.pop(which, store.id) # prior to choose original region
                        store.add(region)
                        # try to reduce upper bound
                        if flowRatio(store, 0) > 1 + ratio or flowRatio(store, 1) > 1 + ratio:
                            dom_regions.push(which, region)
                            store.remove(region)
                            break
                    else: # no region can to be migrated
                        logger.debug("type %d empty" % which)
                        break
                logger.debug("step3: place remaining dom_regions, store id %s: (%.2f, %.2f)" % (store.id, flowRatio(store, 0), flowRatio(store, 1)))

        # step 4: final placing
        for which in range(load_nums):
            store_infos.sort(key=lambda s: s.act_vals_sum[which])
            for store in store_infos:
                while flowRatio(store, which) < 1 and not dom_regions.empty(which):
                    region = dom_regions.pop(which, store.id)
                    store.add(region)

        assert dom_regions.empty(0) and dom_regions.empty(1)
        return dom_regions.buildSolution()    

    @classmethod
    def greedyMultiDim(cls, store_infos, ratio):
        load_nums = store_infos[0].vals_num
        # calc expected loads of stores
        val_expected = []
        for which in range(load_nums):
            vals = [store.act_vals_sum[which] for store in store_infos]
            val_expected.append(np.mean(vals))
        
        #normalize load
        region_list = []
        for store in store_infos:
            store.vals_sum = np.true_divide(store.vals_sum, val_expected)
            store.act_vals_sum = np.true_divide(store.act_vals_sum, val_expected)
            for region in store.regions.values():
                region.vals = np.true_divide(region.vals, val_expected)
                if np.max(region.vals) > ratio:
                    logger.debug("find high load region %s: %s" % (region.id, region.vals))
                region.pinned = False
            
            store.sortRegionByMaxLoad()
            region_list.extend(store.sorted_regions)

            store.regions = {}
            store.vals_sum = [0] * load_nums
            store.act_vals_sum = [0] * load_nums
        
        store_map = {}
        for store in store_infos:
            store_map[store.id] = store

        #local pinning
        for store in store_infos:
            local_region_list = store.sorted_regions
            new_vals_sum = [0] * load_nums
            for i in range(len(local_region_list)):
                min_load_diff = 100000
                switched_index = -1
                for j in range(i, len(local_region_list)):
                    region = local_region_list[j]
                    new_loads = np.add(new_vals_sum, region.vals)
                    new_load_max = np.max(new_loads)
                    new_load_min = np.min(new_loads)
                    if new_load_max > 1:
                        break
                    if new_load_max - new_load_min <= ratio:
                        switched_index = j
                        break
                if switched_index != -1:
                    switched_region = local_region_list[switched_index]
                    if switched_index != i:
                        local_region_list.pop(switched_index)
                        local_region_list.insert(i, switched_region)
                    switched_region.pinned = True
                    # store_map[switched_region.src_sid].remove(switched_region)
                    new_vals_sum = np.add(new_vals_sum, switched_region.vals)

        migrated_regions = []

        #Adjust the region_list to distribute the load evenly
        new_vals_sum = [0] * load_nums
        cur_store_index = 0
        cur_store = store_infos[0]
        left_regions = []
        for i in range(len(region_list)):
            min_load_diff = 100000
            switched_index = i
            err_pinned_tag = False
            for j in range(i, len(region_list)):
                region = region_list[j]
                if region.pinned:
                    if cur_store == None:
                        print("err pinning")
                        err_pinned_tag = True
                        break
                    elif region.src_sid == cur_store.id:
                        switched_index = j
                        break
                    else:
                        continue
                new_loads = np.add(new_vals_sum, region.vals)
                new_load_max = np.max(new_loads)
                new_load_min = np.min(new_loads)
                if new_load_max - new_load_min <= ratio:
                    switched_index = j
                    break
                if min_load_diff > new_load_max - new_load_min:
                    min_load_diff = new_load_max - new_load_min
                    switched_index = j

            if err_pinned_tag:
                left_regions.append(region_list[i])
                continue
            
            switched_region = region_list[switched_index]
            if switched_index != i:
                region_list.pop(switched_index)
                region_list.insert(i, switched_region)
                
            # store_map[switched_region.src_sid].remove(switched_region)
            new_vals_sum = np.add(new_vals_sum, switched_region.vals)

            #dispatch region
            if cur_store:
                new_loads = np.add(cur_store.act_vals_sum, switched_region.vals)
                if np.max(new_loads) > 1 + ratio:   # move to next store
                    logger.debug("after balanced, sid %s with load %s" % (cur_store.id, cur_store.act_vals_sum))
                    cur_store_index += 1
                    if cur_store_index < len(store_infos):
                        cur_store = store_infos[cur_store_index]
                    else:
                        print("last store")
                        cur_store = None
                if cur_store:
                    cur_store.add(switched_region)
                    switched_region.dst_sid = cur_store.id
                    if switched_region.dst_sid != switched_region.src_sid:
                        migrated_regions.append(switched_region)
            else:
                left_regions.append(switched_region)
        
        #special case caused by numerical calculation error
        for left_region in left_regions:
            print("left region with load %f" % np.max(left_region.vals))
            min_store = None
            min_load = 100000
            for store in store_infos:
                new_loads = np.add(store.act_vals_sum, left_region.vals)
                if min_load > np.max(new_loads):
                    min_load = np.max(new_loads)
                    min_store = store
            min_store.add(left_region)
            left_region.dst_sid = min_store.id
            if left_region.dst_sid != left_region.src_sid:
                migrated_regions.append(left_region)

        solution = []
        for region in migrated_regions:
            solution.append([str(region.id), region.src_sid, region.dst_sid])
        return solution

    @classmethod
    def greedyMultiDimWithoutPinning(cls, store_infos, ratio):
        load_nums = store_infos[0].vals_num
        # calc expected loads of stores
        val_expected = []
        for which in range(load_nums):
            vals = [store.act_vals_sum[which] for store in store_infos]
            val_expected.append(np.mean(vals))
        
        #normalize load
        region_list = []
        for store in store_infos:
            store.vals_sum = np.true_divide(store.vals_sum, val_expected)
            store.act_vals_sum = np.true_divide(store.act_vals_sum, val_expected)
            for region in store.regions.values():
                region.vals = np.true_divide(region.vals, val_expected)
                if np.max(region.vals) > ratio:
                    logger.debug("find high load region %s: %s" % (region.id, region.vals))
                region.pinned = False
            
            store.sortRegionByMaxLoad()
            region_list.extend(store.sorted_regions)
        
        store_map = {}
        for store in store_infos:
            store_map[store.id] = store

        #Adjust the region_list to distribute the load evenly
        new_vals_sum = [0] * load_nums
        for i in range(len(region_list)):
            min_load_diff = 100000
            switched_index = i
            break_flag = False
            for j in range(i, len(region_list)):
                region = region_list[j]
                if np.max(region.vals) > ratio:
                    print("error, %f" % np.max(region.vals))
                new_loads = np.add(new_vals_sum, region.vals)
                new_load_max = np.max(new_loads)
                new_load_min = np.min(new_loads)
                if new_load_max - new_load_min <= ratio:
                    switched_index = j
                    break_flag = True
                    break
                if min_load_diff > new_load_max - new_load_min:
                    min_load_diff = new_load_max - new_load_min
                    switched_index = j

            if not break_flag:
                print("not found!! i %d j %d, len %d diff %f" % (i, j, len(region_list) - 1, min_load_diff))

            switched_region = region_list[switched_index]
            if switched_index != i:
                region_list.pop(switched_index)
                region_list.insert(i, switched_region)
                
            store_map[switched_region.src_sid].remove(switched_region)
            new_vals_sum = np.add(new_vals_sum, switched_region.vals)

        migrated_regions = []

        #dispatch region
        i = 0
        for store in store_infos:
            while i < len(region_list) and np.max(store.act_vals_sum) <= 1 + ratio:
                new_loads = np.add(store.act_vals_sum, region_list[i].vals)
                if np.max(new_loads) > 1 + ratio:
                    break
                store.add(region_list[i])
                region_list[i].dst_sid = store.id
                if region_list[i].dst_sid != region_list[i].src_sid:
                    migrated_regions.append(region_list[i])
                i += 1
            logger.debug("after balanced, sid %s with load %s" % (store.id, store.act_vals_sum))
        
        #special case caused by numerical calculation error
        while i < len(region_list):
            min_store = None
            min_load = 100000
            for store in store_infos:
                new_loads = np.add(store.act_vals_sum, region_list[i].vals)
                if min_load > np.max(new_loads):
                    min_load = np.max(new_loads)
                    min_store = store
            min_store.add(region_list[i])
            region_list[i].dst_sid = min_store.id
            if region_list[i].dst_sid != region_list[i].src_sid:
                migrated_regions.append(region_list[i])

        solution = []
        for region in migrated_regions:
            solution.append([str(region.id), region.src_sid, region.dst_sid])
        return solution

    @classmethod
    def greedyMultiDimGreedy(cls, store_infos, ratio):
        load_nums = store_infos[0].vals_num
        # calc expected loads of stores
        val_expected = []
        for which in range(load_nums):
            vals = [store.act_vals_sum[which] for store in store_infos]
            val_expected.append(np.mean(vals))
        
        #normalize load
        for store in store_infos:
            store.vals_sum = np.true_divide(store.vals_sum, val_expected)
            store.act_vals_sum = np.true_divide(store.act_vals_sum, val_expected)
            for region in store.regions.values():
                region.vals = np.true_divide(region.vals, val_expected)
                if np.max(region.vals) > ratio:
                    logger.debug("find high load region %s: %s" % (region.id, region.vals))
        
        
        store_map = {}
        for store in store_infos:
            store_map[store.id] = store

        #Adjust the region_list to distribute the load evenly
        new_vals_sum = [0] * load_nums
        for i in range(len(region_list)):
            min_load_diff = 100000
            switched_index = i
            for j in range(i, len(region_list)):
                region = region_list[j]
                new_loads = np.add(new_vals_sum, region.vals)
                new_load_max = np.max(new_loads)
                new_load_min = np.min(new_loads)
                if new_load_max - new_load_min <= ratio:
                    switched_index = j
                    break
                if min_load_diff > new_load_max - new_load_min:
                    min_load_diff = new_load_max - new_load_min
                    switched_index = j

            switched_region = region_list[switched_index]
            if switched_index != i:
                region_list.pop(switched_index)
                region_list.insert(i, switched_region)
                
            store_map[switched_region.src_sid].remove(switched_region)
            new_vals_sum = np.add(new_vals_sum, switched_region.vals)

        migrated_regions = []

        #dispatch region
        i = 0
        for store in store_infos:
            while i < len(region_list) and np.max(store.act_vals_sum) <= 1 + ratio:
                new_loads = np.add(store.act_vals_sum, region_list[i].vals)
                if np.max(new_loads) > 1 + ratio:
                    break
                store.add(region_list[i])
                region_list[i].dst_sid = store.id
                if region_list[i].dst_sid != region_list[i].src_sid:
                    migrated_regions.append(region_list[i])
                i += 1
            logger.debug("after balanced, sid %s with load %s" % (store.id, store.act_vals_sum))
        
        #special case caused by numerical calculation error
        while i < len(region_list):
            min_store = None
            min_load = 100000
            for store in store_infos:
                new_loads = np.add(store.act_vals_sum, region_list[i].vals)
                if min_load > np.max(new_loads):
                    min_load = np.max(new_loads)
                    min_store = store
            min_store.add(region_list[i])
            region_list[i].dst_sid = min_store.id
            if region_list[i].dst_sid != region_list[i].src_sid:
                migrated_regions.append(region_list[i])

        solution = []
        for region in migrated_regions:
            solution.append([str(region.id), region.src_sid, region.dst_sid])
        return solution

    @classmethod
    def LPBalance(cls, load_nums, store_infos, ratio, allow_split, measure_time = True):
        run_times = {}
        run_times["total"] = time.time()

        run_times["preprocess"] = time.time()

        store_nums = len(store_infos)
        region_nums = sum(len(store.regions) for store in store_infos)

        # define location of migrated regions
        store_ids = [store.id for store in store_infos]
        region_ids = []
        for store in store_infos:
            region_ids.extend([str(region.id) for region in store.regions.values()])
        var_names = []
        for sid in store_ids:
            var_names.extend([rid + "_" + sid for rid in region_ids])                
        
        # use current region placement to determine the location cost
        location_costs = dict(zip(var_names, [1] * (region_nums * store_nums)))
        for store in store_infos:
            for region in store.regions.values():
                location_costs[str(region.id) + "_" + store.id] = 0
    
        # allow region splitting
        split_rids = []
        # sort regions by specified attribute
        if allow_split:
            for store in store_infos:
                store.sort(0)
                for r in store.sorted_regions:
                    split_rids.append(str(r.id))

        split_var_names = []
        for sid in store_ids:
            split_var_names.extend([rid + "_" + sid for rid in split_rids])
        non_split_var_names = location_costs.copy()
        for var in split_var_names:
            del non_split_var_names[var]
        non_split_var_names = non_split_var_names.keys()

        # calc expected loads of stores
        val_expected = []
        val_upper = []
        val_lower = []
        for which in range(load_nums):
            vals = [store.act_vals_sum[which] for store in store_infos]
            val_expected.append(np.mean(vals))
            val_upper.append({})
            val_lower.append({})
            for store in store_infos:
                upper = val_expected[which] * (1 + ratio)
                lower = val_expected[which] * (1 - ratio)
                val_upper[which][store.id] = upper - store.act_vals_sum[which] + store.vals_sum[which]
                val_lower[which][store.id] = lower - store.act_vals_sum[which] + store.vals_sum[which]
        
        # region loads
        region_loads = [{} for i in range(load_nums)]
        for store in store_infos:
            for region in store.regions.values():
                for j in range(load_nums):
                    region_loads[j][str(region.id)] = region.vals[j]

        run_times["preprocess"] = time.time() - run_times["preprocess"]
        run_times["define"] = time.time()

        # define LP problem
        prob = pulp.LpProblem("Load Balance Problem", pulp.LpMinimize)

        # define LP vars
        lp_vars = pulp.LpVariable.dicts("x", split_var_names, 0, 1)
        nsplit_lp_vars = pulp.LpVariable.dicts("x", non_split_var_names, 0, 1, pulp.LpInteger)
        lp_vars.update(nsplit_lp_vars)

        # objective function
        prob += pulp.lpSum([location_costs[var] * lp_vars[var] for var in var_names]), "MigrationOverhead"
        for rid in region_ids:
            prob += pulp.lpSum([lp_vars[rid + "_" + sid] for sid in store_ids]) == 1, "Region%sLocationLimit" % rid
        
        # store load limitation
        for sid in store_ids:
            for load_i in range(load_nums):
                prob += pulp.lpSum([lp_vars[rid + "_" + sid] * region_loads[load_i][rid] for rid in region_ids]) >= val_lower[load_i][sid], "Store%sLoad%dLowerLimit" % (sid, load_i)
                prob += pulp.lpSum([lp_vars[rid + "_" + sid] * region_loads[load_i][rid] for rid in region_ids]) <= val_upper[load_i][sid], "Store%sLoad%dUpperLimit" % (sid, load_i)
        
        run_times["define"] = time.time() - run_times["define"]

        prob.writeLP("test.lp")
        if measure_time:
            logger.debug("finish generate data, used times:" + str(run_times))

        run_times["solve"] = time.time()
        prob.solve() # pulp.PULP_CBC_CMD(threads=4)
        run_times["solve"] = time.time() - run_times["solve"]
        run_times["total"] = time.time() - run_times["total"]

        logger.debug("Status:" + pulp.LpStatus[prob.status])
        if measure_time:
            logger.debug("run time:" + str(run_times))
            return
        
        solved_x = {}
        for i in range(region_nums * store_nums):
            solved_x[var_names[i]] = (pulp.value(lp_vars[var_names[i]]))

        for rid in split_rids:
            vals = []
            for sid in store_ids:
                vals.append(pulp.value(lp_vars[rid + "_" + sid]))
            if max(vals) != 1.0:
                logger.debug("splitted rid %s:" % rid + " ".join([str(v) for v in vals]))
        
        aft_max_mean = []
        for sid in store_ids:
            for load_i in range(load_nums):
                load = [solved_x[rid + "_" + sid] * region_loads[load_i][rid] for rid in region_ids]
                load = sum(load) + store.act_vals_sum[load_i] - store.vals_sum[load_i]
                aft_max_mean.append(load / val_expected[load_i])
                logger.debug("store %s, [load %d] expected %.2f, calc %.2f, percent %.2f" % (sid, load_i, val_expected[load_i], load, load / val_expected[load_i]))
        logger.debug("migration cost %.2f" % pulp.value(prob.objective))

        # build solution
        ret = []
        for rid in region_ids:
            cost = 0
            src_id = "-1"
            dst_ids = []
            for sid in store_ids:
                if location_costs[rid + "_" + sid] == 0:
                    src_id = sid
                if solved_x[rid + "_" + sid] != 0:
                    dst_ids.append(sid)
                cost += solved_x[rid + "_" + sid] * location_costs[rid + "_" + sid]
            if cost != 0:
                tmp = [rid, src_id]
                tmp.extend(dst_ids)
                ret.append(tmp)
        return ret, np.max(aft_max_mean)

def calcMaxMeanRatio(load_nums, store_infos, show_all_dims = True):
    ret_vals = []
    for which in range(load_nums):
        vals = [store.act_vals_sum[which] for store in store_infos]
        ret_vals.append(np.max(vals) / np.mean(vals))
    if show_all_dims:
        return ret_vals
    else:
        return max(ret_vals)

def calcMinMeanRatio(load_nums, store_infos, show_all_dims = True):
    ret_vals = []
    for which in range(load_nums):
        vals = [store.act_vals_sum[which] for store in store_infos]
        ret_vals.append(np.min(vals) / np.mean(vals))
    if show_all_dims:
        return ret_vals
    else:
        return max(ret_vals)

class RegionInfoGenerator(object):
    def __init__(self, load_nums, max_load, nums, max_flow_rate = 1.0):
        self.load_nums = load_nums
        self.store_nums = nums[0]
        self.migrate_nums = nums[1]
        self.max_flow_rate = max_flow_rate

        self.id = 0
        self.expect_loads = [max_load] * self.load_nums
    
    def allocateId(self):
        self.id += 1
        return str(self.id)

    def allocateLoad(self, total, upper):
        if total < upper:
            upper = total
        load = random.uniform(0, upper)
        # if load > upper / 2:
        #     load = upper * 0.99
        # else:
        #     load = upper * 0.01
        return load

    def printStoreInfo(self, store_infos):
        store_ids = []
        stores_flows = []
        regions_flows = []
        for i in range(self.load_nums) :
            stores_flows.append([])     # flow_bytes, flow_keys
            regions_flows.append({})     # flow_bytes, flow_keys
        flow_dist = []            # (flow, rid)

        # init data
        for store_data in store_infos:
            store_ids.append(store_data.id)
            rids = []
            for region in store_data.regions.values():
                rid = int(region.id)
                rid_item = []
                for i in range(self.load_nums):
                    regions_flows[i][rid] = region.vals[i]
                    rid_item.append(regions_flows[i][rid])
                rid_item.append(rid)
                rids.append(rid_item)
            flow_dist.append(sorted(rids))
            
            for i in range(self.load_nums):
                stores_flows[i].append(store_data.act_vals_sum[i])

        exp_loads = []
        for i in range(self.load_nums):
            exp_loads.append(np.mean(stores_flows[i]))
        
        index = 0
        for flows in flow_dist:
            store_id = store_ids[index]
            print("store id: %s, " % (store_id), end='')
            for i in range(self.load_nums):
                print("act load%d %.2fK(%.2f%%), " % (i, stores_flows[i][index], stores_flows[i][index] / exp_loads[i] * 100))
            index += 1
            i = 0
            print("hot regions: (total %d)" % len(flows))
            while i < 100 and i < len(flows):
                for l in range(self.load_nums):
                    print("load%d %.2fK(%.2f%%)" % (l, flows[-1 - i][l], flows[-1 - i][l] / exp_loads[l] * 100.0))
                print("rid: %d", flows[-1 - i][-1])
                i += 1
            print()
    
    def dumpStoreInfo(self, store_infos, fout_name):
        fout = open(fout_name, "w")
        fout.write("%d\n"%len(store_infos))
        for store in store_infos:
            fout.write("%s\n"%store.id)
            fout.write("%d\n"%len(store.regions))
            for region in store.regions.values():
                fout.write("%s\n"%region.id)
                fout.write("%s\n"%str(region.vals))
        fout.close()
    
    def loadStoreInfo(self, fin_name):
        fin = open(fin_name, "r")
        store_infos = []
        for _ in range(int(fin.readline())):
            store_info = StoreInfo(self.load_nums, fin.readline()[:-1])
            for _ in range(int(fin.readline())):
                rid = fin.readline()[:-1]
                vals = eval(fin.readline())
                region = RegionInfo(rid, vals)
                store_info.add(region)
            store_infos.append(store_info)
        fin.close()
        return store_infos
    
    def generate(self):
        used_time = time.time()
        total_loads = [self.expect_loads[0]] * self.load_nums
        limited_load = self.expect_loads[0] * self.max_flow_rate / self.store_nums
        
        store_infos = []
        for i in range(self.store_nums):
            store = StoreInfo(self.load_nums, self.allocateId())
            store_infos.append(store)
        
        while np.max(total_loads) > limited_load:
            cur_loads = []
            for k in range(self.load_nums):
                cur_load = self.allocateLoad(total_loads[k], limited_load)
                cur_loads.append(cur_load)
                total_loads[k] -= cur_load
            region = RegionInfo(self.allocateId(), cur_loads)
            src_sid = random.randint(0, self.store_nums - 1)
            store = store_infos[src_sid]
            region.src_sid = store.id
            store.add(region, False)
        if np.max(total_loads) > 0:
            cur_loads = []
            for k in range(self.load_nums):
                cur_load = total_loads[k]
                cur_loads.append(cur_load)
                total_loads[k] -= cur_load
            region = RegionInfo(self.allocateId(), cur_loads)
            src_sid = random.randint(0, self.store_nums - 1)
            store = store_infos[src_sid]
            region.src_sid = store.id
            store.add(region, False)
        
        moved_rid = {}
        for i in range(self.migrate_nums):
            src_sid = random.randint(0, self.store_nums - 1)
            dst_sid = random.randint(0, self.store_nums - 1)
            while dst_sid == src_sid:
                dst_sid = random.randint(0, self.store_nums - 1)

            src_store = store_infos[src_sid]
            dst_store = store_infos[dst_sid]

            if len(src_store.regions) == 0:
                continue
            region = src_store.getRandomRegion()
            # while region.id in moved_rid:
            #     region = src_store.getRandomRegion()
            # moved_rid[region.id] = True

            StoreInfo.migrate(region, src_store, dst_store)
            region.src_sid = dst_store.id
        
        used_time = time.time() - used_time
        logger.debug("generate takes %fs" % used_time)
        return store_infos

class LoadBalanceSimulator(object):
    def simulate(self, opt, store_nums, tolerant_rate, allow_split = False):
        region_nums = 10
        migrate_nums = int(store_nums * region_nums * 0.5)
        load_nums = 2
        max_load = 1

        logger.debug("config: store_nums %d, region_nums %d per store, tolerant_rate %f, allow_split %s" % (store_nums, region_nums, tolerant_rate, str(allow_split)))

        ret = {}
        if opt["limit-flow"]:
            gen = RegionInfoGenerator(load_nums, max_load, [store_nums, migrate_nums], max_flow_rate = tolerant_rate)
        else:
            gen = RegionInfoGenerator(load_nums, max_load, [store_nums, migrate_nums], max_flow_rate = 1.0)
        store_infos = gen.generate()
        if "load" in opt:
            store_infos = gen.loadStoreInfo("store.txt")
        gen.dumpStoreInfo(store_infos, "store.txt")

        if "print" in opt:
            gen.printStoreInfo(store_infos)
        ret["pre_max_mean"] = calcMaxMeanRatio(load_nums, store_infos, False)
        logger.debug("before balance, max/mean: %s" % str(ret["pre_max_mean"]))
        ret["pre_min_mean"] = calcMinMeanRatio(load_nums, store_infos, False)
        logger.debug("before balance, min/mean: %s" % str(ret["pre_min_mean"]))

        used_time = time.time()

        if opt["alg"].upper() == "ILP":
            # logger.setLevel(logging.DEBUG)
            solution, aft_max_mean = StoreInfo.LPBalance(load_nums, store_infos, tolerant_rate, allow_split, measure_time = False)
            ret["migrate_nums"] = len(solution)
            ret["aft_max_mean"] = aft_max_mean
            ret["aft_min_mean"] = aft_max_mean
        elif opt["alg"].upper() == "GREEDY-SINGLE":
            solution, b = StoreInfo.balanceSingle(store_infos, tolerant_rate, 0, enable_splitting = False)
            ret["migrate_nums"] = len(solution)
            solution, b = StoreInfo.balanceSingle(store_infos, tolerant_rate, 1, enable_splitting = False)
            ret["migrate_nums"] += len(solution)
        elif opt["alg"].upper() == "GREEDY-GLOBAL":
            solution = StoreInfo.greedy(store_infos, tolerant_rate)
            logger.debug("migrate region nums: %d" % len(solution))
            ret["migrate_nums"] = len(solution)
        elif opt["alg"].upper() == "GREEDY-GLOBAL-SPLIT":
            # logger.setLevel(logging.DEBUG)
            solution = StoreInfo.greedySplit(store_infos, tolerant_rate)
            logger.debug("migrate region nums: %d" % len(solution))
            ret["migrate_nums"] = len(solution)
            # logger.error("invalid alg name:" + opt["alg"])
        elif opt["alg"].upper() == "GREEDY-MULTI":
            solution = StoreInfo.greedyMultiDimWithoutPinning(store_infos, tolerant_rate)
            logger.debug("migrate region nums: %d" % len(solution))
            ret["migrate_nums"] = len(solution)
        elif opt["alg"].upper() == "GREEDY-MULTI-GREEDY":
            solution = StoreInfo.balanceMultiGreedy(store_infos, tolerant_rate)
            logger.debug("migrate region nums: %d" % len(solution))
            ret["migrate_nums"] = len(solution)
        elif opt["alg"].upper() == "GREEDY-MULTI-GREEDY-GENERAL":
            solution = StoreInfo.balanceMultiGreedyGeneral(store_infos, tolerant_rate)
            logger.debug("migrate region nums: %d" % len(solution))
            ret["migrate_nums"] = len(solution)

        used_time = time.time() - used_time
        ret["used_time"] = used_time

        if "print" in opt:
            gen.printStoreInfo(store_infos)
        if opt["alg"].upper() != "ILP":
            ret["aft_max_mean"] = calcMaxMeanRatio(load_nums, store_infos, False)
            ret["aft_min_mean"] = calcMinMeanRatio(load_nums, store_infos, False)
        logger.debug("after balance, max/mean: %s" % str(ret["aft_max_mean"]))
        logger.debug("after balance, min/mean: %s" % str(ret["aft_min_mean"]))
        return ret

def ILPTest():
    print("argv:", sys.argv)
    store_nums = int(sys.argv[1])
    tolerant_rate = float(sys.argv[2])
    allow_split = bool(sys.argv[3])

    sim = LoadBalanceSimulator()
    opt = {"alg":"ILP"}
    sim.simulate(opt, store_nums, tolerant_rate, allow_split)

def greedyTest():
    print("argv:", sys.argv)
    store_nums = int(sys.argv[1])
    tolerant_rate = float(sys.argv[2])
    repeat_nums = 10
    alg = "GREEDY-GLOBAL"
    if len(sys.argv) >= 4:
        repeat_nums = int(sys.argv[3])
    if len(sys.argv) >= 5:
        alg = sys.argv[4]
        # if alg.lower().find("single") != -1:
        #     alg = "GREEDY-SINGLE"
        # elif alg.lower().find("global-limit") != -1:
        #     alg = "GREEDY-GLOBAL"
        # elif alg.lower().find("ilp") != -1:
        #     alg = "ILP"
        # elif alg.lower().find("global-split") != -1 or alg.lower().find("greedy") != -1:
        #     alg = "GREEDY-GLOBAL-SPLIT"
        # elif alg.lower().find("multi") != -1:
        #     alg = "GREEDY-MULTI"

    vals = {}
    sim = LoadBalanceSimulator()
    opt = {"alg":alg, "limit-flow":True}
    for i in range(repeat_nums):
        val = sim.simulate(opt, store_nums, tolerant_rate, True)
        for key in val:
            if key not in vals:
                vals[key] = []
            vals[key].append(val[key])
    for key in vals:
        print("%s: max %f, avg %f"%(key, np.max(vals[key]), np.mean(vals[key])))    

if __name__ == "__main__":
    # logger.setLevel(logging.ERROR)    
    logger.setLevel(logging.INFO)
    logger.setLevel(logging.DEBUG)

    # ILPTest()
    greedyTest()

