// Copyright 2020 TiKV Project Authors. Licensed under Apache-2.0.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::slice::Iter;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;
use std::time::{Duration, SystemTime};

use kvproto::kvrpcpb::KeyRange;
use kvproto::metapb::Peer;

use rand::Rng;

use tikv_util::collections::HashMap;
use tikv_util::config::Tracker;

use txn_types::Key;

use crate::store::worker::split_config::DEFAULT_SAMPLE_NUM;
use crate::store::worker::{FlowStatistics, SplitConfig, SplitConfigManager};

pub const TOP_N: usize = 10;

pub struct RatioSplitInfo
{
    pub dim_id: u64,
    pub ratio: f64,
    pub rw_type: u64, // 0 => read, other => write
    pub create_time: Instant,
}

impl RatioSplitInfo {
    fn new() -> RatioSplitInfo {
        RatioSplitInfo {
            dim_id: 0,
            ratio: 0.0,
            rw_type: 0,
            create_time: Instant::now(),
        }
    }
}

#[derive(Default, Debug, Clone)]
pub struct RequestInfo {
    pub start_key: Vec<u8>,
    pub end_key: Vec<u8>,
    pub bytes: usize,
    pub keys: usize,
}

impl RequestInfo {
    fn get_load(&self, id: u64) -> f64 {
        if id == 0 {
            self.bytes as f64
        } else {
            1.0
        }
    }
}

pub struct SplitInfo {
    pub region_id: u64,
    pub split_keys: Vec<Vec<u8>>,
    pub peer: Peer,
}

pub struct Sample {
    pub key: Vec<u8>,
    pub left: i32,
    pub contained: i32,
    pub right: i32,
}

impl Sample {
    fn new(key: &[u8]) -> Sample {
        Sample {
            key: key.to_owned(),
            left: 0,
            contained: 0,
            right: 0,
        }
    }
}

// It will return prefix sum of iter. `read` is a function to be used to read data from iter.
fn prefix_sum<F, T>(iter: Iter<T>, read: F) -> Vec<usize>
where
    F: Fn(&T) -> usize,
{
    let mut pre_sum = vec![];
    let mut sum = 0;
    for item in iter {
        sum += read(&item);
        pre_sum.push(sum);
    }
    pre_sum
}

// It will return sample_num numbers by sample from lists.
// The list in the lists has the length of N1, N2, N3 ... Np ... NP in turn.
// Their prefix sum is pre_sum and we can get mut list from lists by get_mut.
// Take a random number d from [1, N]. If d < N1, select a data in the first list with an equal probability without replacement;
// If N1 <= d <(N1 + N2), then select a data in the second list with equal probability without replacement;
// and so on, repeat m times, and finally select sample_num pieces of data from lists.
fn sample<F, T>(
    sample_num: usize,
    pre_sum: &[usize],
    mut lists: Vec<T>,
    get_mut: F,
) -> Vec<KeyRange>
where
    F: Fn(&mut T) -> &mut Vec<KeyRange>,
{
    let mut rng = rand::thread_rng();
    let mut key_ranges = vec![];
    let high_bound = pre_sum.last().unwrap();
    for _num in 0..sample_num {
        let d = rng.gen_range(0, *high_bound) as usize;
        let i = match pre_sum.binary_search(&d) {
            Ok(i) => i,
            Err(i) => i,
        };
        let list = get_mut(&mut lists[i]);
        let j = rng.gen_range(0, list.len()) as usize;
        key_ranges.push(list.remove(j)); // Sampling without replacement
    }
    key_ranges
}

// RegionInfo will maintain key_ranges with sample_num length by reservoir sampling.
// And it will save qps num and peer.
#[derive(Debug, Clone)]
pub struct RegionInfo {
    pub sample_num: usize,
    pub qps: usize,
    pub bytes: usize,
    pub keys: usize,
    pub peer: Peer,
    pub key_ranges: Vec<KeyRange>,
    pub req_infos: Vec<RequestInfo>,
}

impl RegionInfo {
    fn new(sample_num: usize) -> RegionInfo {
        RegionInfo {
            sample_num,
            qps: 0,
            bytes: 0,
            keys: 0,
            key_ranges: Vec::with_capacity(sample_num),
            peer: Peer::default(),
            req_infos: Vec::with_capacity(sample_num),
        }
    }

    fn get_qps(&self) -> usize {
        self.qps
    }

    fn get_key_ranges_mut(&mut self) -> &mut Vec<KeyRange> {
        &mut self.key_ranges
    }

    fn add_key_ranges(&mut self, key_ranges: Vec<KeyRange>) {
        self.qps += key_ranges.len();
        for key_range in key_ranges {
            if self.key_ranges.len() < self.sample_num {
                self.key_ranges.push(key_range);
            } else {
                let i = rand::thread_rng().gen_range(0, self.qps) as usize;
                if i < self.sample_num {
                    self.key_ranges[i] = key_range;
                }
            }
        }
    }

    fn get_req_infos_mut(&mut self) -> &mut Vec<RequestInfo> {
        &mut self.req_infos
    }

    fn add_req_infos(&mut self, req_infos: Vec<RequestInfo>) {
        self.qps += req_infos.len();
        for req_info in req_infos {
            self.bytes += req_info.bytes;
            self.keys += req_info.keys;
            if self.req_infos.len() < self.sample_num {
                self.req_infos.push(req_info);
            } else {
                let i = rand::thread_rng().gen_range(0, self.qps) as usize;
                if i < self.sample_num {
                    self.req_infos[i] = req_info;
                }
            }
        }
    }

    fn update_peer(&mut self, peer: &Peer) {
        if self.peer != *peer {
            self.peer = peer.clone();
        }
    }
}

pub struct Recorder {
    pub detect_num: u64,
    pub peer: Peer,
    pub key_ranges: Vec<Vec<KeyRange>>,
    pub req_infos: Vec<Vec<RequestInfo>>,
    pub times: u64,
    pub create_time: SystemTime,
}

impl Recorder {
    fn new(detect_num: u64) -> Recorder {
        Recorder {
            detect_num,
            peer: Peer::default(),
            key_ranges: vec![],
            req_infos: vec![],
            times: 0,
            create_time: SystemTime::now(),
        }
    }

    fn record(&mut self, key_ranges: Vec<KeyRange>) {
        self.times += 1;
        self.key_ranges.push(key_ranges);
    }

    fn record_req_infos(&mut self, req_infos: Vec<RequestInfo>) {
        self.times += 1;
        self.req_infos.push(req_infos);
    }

    fn update_peer(&mut self, peer: &Peer) {
        if self.peer != *peer {
            self.peer = peer.clone();
        }
    }

    fn is_ready(&self) -> bool {
        self.times >= self.detect_num
    }

    fn collect(&mut self, config: &SplitConfig) -> Vec<u8> {
        let pre_sum = prefix_sum(self.key_ranges.iter(), Vec::len);
        let key_ranges = self.key_ranges.clone();
        let mut samples = sample(config.sample_num, &pre_sum, key_ranges, |x| x)
            .iter()
            .map(|key_range| Sample::new(&key_range.start_key))
            .collect();
        for key_ranges in &self.key_ranges {
            for key_range in key_ranges {
                Recorder::sample(&mut samples, &key_range);
            }
        }
        Recorder::split_key(
            samples,
            config.split_balance_score,
            config.split_contained_score,
            config.sample_threshold,
        )
    }

    fn choose_bounds(&self, mut req_infos: Vec<RequestInfo>, ratio_split_info: &RatioSplitInfo, reverse: bool) -> (Vec<Vec<u8>>, Vec<RequestInfo>) {
        if !reverse {
            req_infos.sort_by(|a, b| a.start_key.cmp(&b.start_key));
        } else {
            req_infos.sort_by(|a, b| b.end_key.cmp(&a.end_key));
        }

        let mut sum: f64 = 0.0;
        if ratio_split_info.dim_id == 0 {   // IO dimension: bytes rate
            for req_info in req_infos.iter() {
                sum += req_info.bytes as f64;
            }
        } else {    // CPU dimension: qps
            sum = req_infos.len() as f64;
        }
        
        let splitted_ratios = {
            let mut ratios = vec![];
            let mut ratio = ratio_split_info.ratio;
            while ratio < 1.0 {
                ratios.push(ratio);
                ratio += ratio_split_info.ratio;
            }
            ratios
        };

        let target_loads = if !reverse {
            let res: Vec<f64> = splitted_ratios.iter().map(|ratio| ratio * sum).collect();
            res
        } else {
            let res: Vec<f64> = splitted_ratios.iter().map(|ratio| (1.0 - ratio) * sum).collect();
            res
        };
        
        let mut target_keys = vec![];
        let mut cur_target = 0;
        let mut cur_load = 0.0;
        for i in 0..req_infos.len() {
            let req_info = &req_infos[i];
            cur_load += req_info.get_load(ratio_split_info.dim_id);
            while cur_target < target_loads.len() && cur_load >= target_loads[cur_target] {
                let key = if !reverse {
                    &req_info.start_key
                } else {
                    &req_info.end_key
                };
                target_keys.push(key.clone());
                cur_target += 1;
                info!("choose_bound"; "reverse" => reverse, "cur_load" => cur_load, "cur_target" => cur_target, "total_load" => sum);
            }
            if cur_target >= target_loads.len() {
                break;
            }
        }

        (target_keys, req_infos)
    }

    fn choose_middle(&self, req_infos: &Vec<RequestInfo>, left_bound: &Vec<u8>, right_bound: &Vec<u8>) -> Vec<u8> {
        let mut target_key = left_bound;
        
        // the most proper split-key is in [left_bound, right_bound], we choose the middle key as the split-key
        let mut contained_num = 0;
        for req_info in req_infos {
            if req_info.start_key.cmp(&left_bound) == Ordering::Greater && req_info.end_key.cmp(&right_bound) == Ordering::Less {
                contained_num += 1;
            }
            if req_info.start_key.cmp(&right_bound) == Ordering::Greater {
                break;
            }
        }
        
        let target = contained_num / 2;
        let mut current = 0;
        for req_info in req_infos {
            if req_info.start_key.cmp(&left_bound) == Ordering::Greater && req_info.end_key.cmp(&right_bound) == Ordering::Less {
                current += 1;
                if current >= target {
                    target_key = &req_info.start_key;
                    break;
                }
            }
            if req_info.start_key.cmp(&right_bound) == Ordering::Greater {
                break;
            }
        }
        info!("choose_middle in ratio based splitting"; "split_key" => format!("{:?}", hex::encode_upper(&target_key)), "contained candidate ranges" => contained_num);

        target_key.clone()
    }

    fn dedup_keys(&self, input: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        let mut output = vec![];
        if input.len() >= 1 {
            output.push(input[0].clone());
        }
        let mut slow = 0;
        let mut fast = 1;
        while fast < input.len() {
            if output[slow].cmp(&input[fast]) != Ordering::Equal {
                output.push(input[fast].clone());
                slow += 1;
            }
            fast += 1;
        }
        output
    }

    fn ratio_split(&mut self, _config: &SplitConfig, ratio_split_info: &RatioSplitInfo) -> Vec<Vec<u8>> {
        let mut req_infos = vec![];
        for req_infos_part in &mut self.req_infos {
            req_infos.append(req_infos_part);
        }

        let (right_bounds, req_infos) = self.choose_bounds(req_infos, ratio_split_info, true);
        let (left_bounds, req_infos) = self.choose_bounds(req_infos, ratio_split_info, false);

        if left_bounds.len() == 0 || right_bounds.len() == 0 || left_bounds.len() != right_bounds.len() {
            warn!("choose_bounds does not work in ratio based splitting"; "left_bounds len" => left_bounds.len(), "right_bounds len" => right_bounds.len());
            return vec![];
        }

        // use middle key of each range as the splitted key.
        let mut target_keys = vec![];
        for i in 0..left_bounds.len() {
            target_keys.push(self.choose_middle(&req_infos, &left_bounds[i], &right_bounds[i]));
        }

        let before_len = target_keys.len();
        let deduped_keys = self.dedup_keys(target_keys);

        info!("ratio split region"; "dim id" => ratio_split_info.dim_id, "ratio" => ratio_split_info.ratio, "before_dedup len" => before_len, "after_dedup len" => deduped_keys.len());
        
        deduped_keys
    }

    fn sample(samples: &mut Vec<Sample>, key_range: &KeyRange) {
        for mut sample in samples.iter_mut() {
            let order_start = if key_range.start_key.is_empty() {
                Ordering::Greater
            } else {
                sample.key.cmp(&key_range.start_key)
            };

            let order_end = if key_range.end_key.is_empty() {
                Ordering::Less
            } else {
                sample.key.cmp(&key_range.end_key)
            };

            if order_start == Ordering::Greater && order_end == Ordering::Less {
                sample.contained += 1;
            } else if order_start != Ordering::Greater {
                sample.right += 1;
            } else {
                sample.left += 1;
            }
        }
    }

    fn split_key(
        samples: Vec<Sample>,
        split_balance_score: f64,
        split_contained_score: f64,
        sample_threshold: i32,
    ) -> Vec<u8> {
        let mut best_index: i32 = -1;
        let mut best_score = 2.0;
        for index in 0..samples.len() {
            let sample = &samples[index];
            let sampled = sample.contained + sample.left + sample.right;
            if (sample.left + sample.right) == 0 || sampled < sample_threshold {
                continue;
            }
            let diff = (sample.left - sample.right) as f64;
            let balance_score = diff.abs() / (sample.left + sample.right) as f64;
            if balance_score >= split_balance_score {
                continue;
            }
            let contained_score = sample.contained as f64 / sampled as f64;
            if contained_score >= split_contained_score {
                continue;
            }
            let final_score = balance_score + contained_score;
            if final_score < best_score {
                best_index = index as i32;
                best_score = final_score;
            }
        }
        if best_index >= 0 {
            return samples[best_index as usize].key.clone();
        }
        return vec![];
    }
}

#[derive(Clone, Debug)]
pub struct ReadStats {
    pub flows: HashMap<u64, FlowStatistics>,
    pub region_infos: HashMap<u64, RegionInfo>,
    pub sample_num: usize,
    pub rw_type: u64,
}

impl ReadStats {
    pub fn default() -> ReadStats {
        ReadStats {
            sample_num: DEFAULT_SAMPLE_NUM,
            region_infos: HashMap::default(),
            flows: HashMap::default(),
            rw_type: 0,
        }
    }

    pub fn default_write() -> ReadStats {
        ReadStats {
            sample_num: DEFAULT_SAMPLE_NUM,
            region_infos: HashMap::default(),
            flows: HashMap::default(),
            rw_type: 1,
        }
    }

    pub fn add_qps(&mut self, region_id: u64, peer: &Peer, key_range: KeyRange) {
        self.add_qps_batch(region_id, peer, vec![key_range]);
    }

    pub fn add_qps_batch(&mut self, region_id: u64, peer: &Peer, key_ranges: Vec<KeyRange>) {
        let num = self.sample_num;
        let region_info = self
            .region_infos
            .entry(region_id)
            .or_insert_with(|| RegionInfo::new(num));
        region_info.update_peer(peer);
        region_info.add_key_ranges(key_ranges);
    }

    pub fn add_req_info(&mut self, region_id: u64, peer: &Peer, req_info: RequestInfo) {
        self.add_req_info_batch(region_id, peer, vec![req_info]);
    }

    pub fn add_req_info_batch(&mut self, region_id: u64, peer: &Peer, req_infos: Vec<RequestInfo>) {
        let num = self.sample_num;
        let region_info = self
            .region_infos
            .entry(region_id)
            .or_insert_with(|| RegionInfo::new(num));
        region_info.update_peer(peer);
        region_info.add_req_infos(req_infos);
    }

    pub fn add_flow(&mut self, region_id: u64, write: &FlowStatistics, data: &FlowStatistics) {
        let flow_stats = self
            .flows
            .entry(region_id)
            .or_insert_with(FlowStatistics::default);
        flow_stats.add(write);
        flow_stats.add(data);
    }

    pub fn is_empty(&self) -> bool {
        self.region_infos.is_empty() && self.flows.is_empty()
    }
}

pub struct AutoSplitController {
    pub recorders: HashMap<u64, Recorder>,
    cfg: SplitConfig,
    cfg_tracker: Tracker<SplitConfig>,
    pub ratio_split_maps: Arc<Mutex<HashMap<u64, RatioSplitInfo>>>,
}

impl AutoSplitController {
    pub fn new(config_manager: SplitConfigManager) -> AutoSplitController {
        AutoSplitController {
            recorders: HashMap::default(),
            cfg: config_manager.value().clone(),
            cfg_tracker: config_manager.0.clone().tracker("split_hub".to_owned()),
            ratio_split_maps: Arc::new(Mutex::new(HashMap::default())),
        }
    }

    pub fn default() -> AutoSplitController {
        AutoSplitController::new(SplitConfigManager::default())
    }

    pub fn flush(&mut self, others: Vec<ReadStats>) -> (Vec<usize>, Vec<SplitInfo>) {
        let mut split_infos = Vec::default();
        let mut top = BinaryHeap::with_capacity(TOP_N as usize);

        // collect from different thread
        let mut region_infos_map = HashMap::default(); // regionID-regionInfos
        let capacity = others.len();
        for other in others {
            for (region_id, region_info) in other.region_infos {
                if region_info.key_ranges.len() >= self.cfg.sample_num {
                    let region_infos = region_infos_map
                        .entry(region_id)
                        .or_insert_with(|| Vec::with_capacity(capacity));
                    region_infos.push(region_info);
                }
            }
        }

        for (region_id, region_infos) in region_infos_map {
            let pre_sum = prefix_sum(region_infos.iter(), RegionInfo::get_qps);

            let qps = *pre_sum.last().unwrap(); // region_infos is not empty
            let num = self.cfg.detect_times;
            if qps > self.cfg.qps_threshold {
                let recorder = self
                    .recorders
                    .entry(region_id)
                    .or_insert_with(|| Recorder::new(num));

                recorder.update_peer(&region_infos[0].peer);

                let key_ranges = sample(
                    self.cfg.sample_num,
                    &pre_sum,
                    region_infos,
                    RegionInfo::get_key_ranges_mut,
                );

                recorder.record(key_ranges);
                if recorder.is_ready() {
                    let key = recorder.collect(&self.cfg);
                    if !key.is_empty() {
                        let split_info = SplitInfo {
                            region_id,
                            split_keys: vec![Key::from_raw(&key).into_encoded()],
                            peer: recorder.peer.clone(),
                        };
                        split_infos.push(split_info);
                        info!("load base split region";"region_id"=>region_id);
                    }
                    self.recorders.remove(&region_id);
                }
            } else {
                self.recorders.remove_entry(&region_id);
            }
            top.push(qps);
        }

        (top.into_vec(), split_infos)
    }

    pub fn process_ratio_split(&mut self, others: Vec<ReadStats>) -> Vec<SplitInfo> {
        let mut split_infos = Vec::default();
        let mut split_maps = self.ratio_split_maps.lock().unwrap();

        // collect from different thread
        let mut region_infos_map = HashMap::default(); // regionID-regionInfos
        let capacity = others.len();
        for other in others {
            for (region_id, region_info) in other.region_infos {
                if split_maps.contains_key(&region_id) {
                    let ratio_split_info = split_maps.entry(region_id).or_insert_with(|| RatioSplitInfo::new());
                    if ratio_split_info.rw_type == other.rw_type {
                        let region_infos = region_infos_map
                            .entry(region_id)
                            .or_insert_with(|| Vec::with_capacity(capacity));
                        region_infos.push(region_info);
                    }
                }
            }
        }

        for (region_id, region_infos) in region_infos_map {
            let num = self.cfg.detect_times;
            if split_maps.contains_key(&region_id) {
                let ratio_split_info = split_maps.entry(region_id).or_insert_with(|| RatioSplitInfo::new());

                let recorder = self
                    .recorders
                    .entry(region_id)
                    .or_insert_with(|| Recorder::new(num));

                recorder.update_peer(&region_infos[0].peer);

                let mut req_infos = vec![];
                for mut region_info in region_infos {
                    req_infos.append(region_info.get_req_infos_mut());
                }

                recorder.record_req_infos(req_infos);

                if recorder.is_ready() {
                    let split_keys = recorder.ratio_split(&self.cfg, ratio_split_info);
                    if !split_keys.is_empty() {
                        // let split_keys: Vec<Vec<u8>> = keys.iter().map(|key| Key::from_raw(&key).into_encoded()).collect();
                        for split_key in &split_keys {
                            info!("ratio split region";"region_id"=>region_id, "split_key"=>format!("{:?}", hex::encode_upper(&split_key)));
                        }
                        let split_info = SplitInfo {
                            region_id,
                            split_keys,
                            peer: recorder.peer.clone(),
                        };
                        split_infos.push(split_info);
                        split_maps.remove(&region_id);
                        info!("ratio split region: success";"region_id"=>region_id);
                    } else {
                        info!("ratio split region: failed";"region_id"=>region_id);
                    }
                    self.recorders.remove(&region_id);
                }
            } else {
                self.recorders.remove_entry(&region_id);
            }
        }

        split_infos
    }

    pub fn clear(&mut self) {
        let interval = Duration::from_secs(self.cfg.detect_times * 2);
        self.recorders
            .retain(|_, recorder| recorder.create_time.elapsed().unwrap() < interval);
    }

    pub fn refresh_cfg(&mut self) {
        if let Some(incoming) = self.cfg_tracker.any_new() {
            self.cfg = incoming.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::util::build_key_range;

    enum Position {
        Left,
        Right,
        Contained,
    }

    impl Sample {
        fn num(&self, pos: Position) -> i32 {
            match pos {
                Position::Left => self.left,
                Position::Right => self.right,
                Position::Contained => self.contained,
            }
        }
    }

    struct SampleCase {
        key: Vec<u8>,
    }

    impl SampleCase {
        fn sample_key(&self, start_key: &[u8], end_key: &[u8], pos: Position) {
            let mut samples = vec![Sample::new(&self.key)];
            let key_range = build_key_range(start_key, end_key, false);
            Recorder::sample(&mut samples, &key_range);
            assert_eq!(
                samples[0].num(pos),
                1,
                "start_key is {:?}, end_key is {:?}",
                String::from_utf8(Vec::from(start_key)).unwrap(),
                String::from_utf8(Vec::from(end_key)).unwrap()
            );
        }
    }

    #[test]
    fn test_pre_sum() {
        let v = vec![1, 2, 3, 4, 5, 6, 7, 8, 9];
        let expect = vec![1, 3, 6, 10, 15, 21, 28, 36, 45];
        let pre = prefix_sum(v.iter(), |x| *x);
        for i in 0..v.len() {
            assert_eq!(expect[i], pre[i]);
        }
    }

    #[test]
    fn test_sample() {
        let sc = SampleCase { key: vec![b'c'] };

        // limit scan
        sc.sample_key(b"a", b"b", Position::Left);
        sc.sample_key(b"a", b"c", Position::Left);
        sc.sample_key(b"a", b"d", Position::Contained);
        sc.sample_key(b"c", b"d", Position::Right);
        sc.sample_key(b"d", b"e", Position::Right);

        // point get
        sc.sample_key(b"a", b"a", Position::Left);
        sc.sample_key(b"c", b"c", Position::Right); // when happened 100 times (a,a) and 100 times (c,c), we will split from c.
        sc.sample_key(b"d", b"d", Position::Right);

        // unlimited scan
        sc.sample_key(b"", b"", Position::Contained);
        sc.sample_key(b"a", b"", Position::Contained);
        sc.sample_key(b"c", b"", Position::Right);
        sc.sample_key(b"d", b"", Position::Right);
        sc.sample_key(b"", b"a", Position::Left);
        sc.sample_key(b"", b"c", Position::Left);
        sc.sample_key(b"", b"d", Position::Contained);
    }

    #[test]
    fn test_hub() {
        let mut hub = AutoSplitController::new(SplitConfigManager::default());
        hub.cfg.qps_threshold = 1;
        hub.cfg.sample_threshold = 0;

        for i in 0..100 {
            let mut qps_stats = ReadStats::default();
            for _ in 0..100 {
                qps_stats.add_qps(1, &Peer::default(), build_key_range(b"a", b"b", false));
                qps_stats.add_qps(1, &Peer::default(), build_key_range(b"b", b"c", false));
            }
            let (_, split_infos) = hub.flush(vec![qps_stats]);
            if (i + 1) % hub.cfg.detect_times == 0 {
                assert_eq!(split_infos.len(), 1);
                assert_eq!(
                    Key::from_encoded(split_infos[0].split_key.clone())
                        .into_raw()
                        .unwrap(),
                    b"b"
                );
            }
        }
    }

    const REGION_NUM: u64 = 1000;
    const KEY_RANGE_NUM: u64 = 1000;

    fn default_qps_stats() -> ReadStats {
        let mut qps_stats = ReadStats::default();
        for i in 0..REGION_NUM {
            for _j in 0..KEY_RANGE_NUM {
                qps_stats.add_qps(i, &Peer::default(), build_key_range(b"a", b"b", false))
            }
        }
        qps_stats
    }

    #[bench]
    fn recorder_sample(b: &mut test::Bencher) {
        let mut samples = vec![Sample::new(b"c")];
        let key_range = build_key_range(b"a", b"b", false);
        b.iter(|| {
            Recorder::sample(&mut samples, &key_range);
        });
    }

    #[bench]
    fn hub_flush(b: &mut test::Bencher) {
        let mut other_qps_stats = vec![];
        for _i in 0..10 {
            other_qps_stats.push(default_qps_stats());
        }
        b.iter(|| {
            let mut hub = AutoSplitController::new(SplitConfigManager::default());
            hub.flush(other_qps_stats.clone());
        });
    }

    #[bench]
    fn qps_scan(b: &mut test::Bencher) {
        let mut qps_stats = default_qps_stats();
        let start_key = Key::from_raw(b"a");
        let end_key = Some(Key::from_raw(b"b"));

        b.iter(|| {
            if let Ok(start_key) = start_key.to_owned().into_raw() {
                let mut key = vec![];
                if let Some(end_key) = &end_key {
                    if let Ok(end_key) = end_key.to_owned().into_raw() {
                        key = end_key;
                    }
                }
                qps_stats.add_qps(
                    0,
                    &Peer::default(),
                    build_key_range(&start_key, &key, false),
                );
            }
        });
    }

    #[bench]
    fn qps_add(b: &mut test::Bencher) {
        let mut qps_stats = default_qps_stats();
        b.iter(|| {
            qps_stats.add_qps(0, &Peer::default(), build_key_range(b"a", b"b", false));
        });
    }
}
