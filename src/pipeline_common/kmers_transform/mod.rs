mod process_subbucket;
pub mod structs;

use crate::hashes::HashableSequence;
use crate::io::concurrent::intermediate_storage::SequenceExtraData;
use crate::io::concurrent::intermediate_storage_single::IntermediateSequencesStorageSingleBucket;
use crate::io::varint::encode_varint_flags;
use crate::types::BucketIndexType;
use crate::utils::chunked_vector::{ChunkedVector, ChunkedVectorPool};
use crate::utils::compressed_read::CompressedRead;
use crate::utils::flexible_pool::FlexiblePool;
use crossbeam::queue::{ArrayQueue, SegQueue};
use parallel_processor::memory_data_size::MemoryDataSize;
use parallel_processor::multi_thread_buckets::BucketsThreadDispatcher;
use parallel_processor::phase_times_monitor::PHASES_TIMES_MONITOR;
use parking_lot::{Mutex, RwLock};
use std::cmp::min;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use structs::ReadRef;

pub const MERGE_BUCKETS_COUNT: usize = 256;
const BUFFER_CHUNK_SIZE: usize = 1024 * 16;

pub struct ReadDispatchInfo<E: SequenceExtraData> {
    pub bucket: BucketIndexType,
    pub hash: u64,
    pub flags: u8,
    pub extra_data: E,
}

pub trait KmersTransformExecutorFactory: Sized + 'static {
    type GlobalExtraData<'a>: Send + Sync + 'a;
    type InputBucketExtraData: SequenceExtraData;
    type IntermediateExtraData: SequenceExtraData;
    type ExecutorType<'a>: KmersTransformExecutor<'a, Self>;

    const FLAGS_COUNT: usize;

    fn new<'a>(global_data: &Self::GlobalExtraData<'a>) -> Self::ExecutorType<'a>;
}

pub trait KmersTransformExecutor<'x, F: KmersTransformExecutorFactory> {
    fn preprocess_bucket(
        &mut self,
        global_data: &F::GlobalExtraData<'x>,
        input_extra_data: F::InputBucketExtraData,
        read: CompressedRead,
    ) -> ReadDispatchInfo<F::IntermediateExtraData>;

    fn maybe_swap_bucket(&mut self, global_data: &F::GlobalExtraData<'x>);

    fn process_group(&mut self, global_data: &F::GlobalExtraData<'x>, reads: &[ReadRef]);

    fn finalize(self, global_data: &F::GlobalExtraData<'x>);
}

pub struct KmersTransform;

impl KmersTransform {
    pub fn parallel_kmers_transform<'a, F: KmersTransformExecutorFactory>(
        file_inputs: Vec<PathBuf>,
        buckets_count: usize,
        threads_count: usize,
        extra_data: F::GlobalExtraData<'a>,
    ) {
        static CURRENT_BUCKETS_COUNT: AtomicU64 = AtomicU64::new(0);

        let files_queue = ArrayQueue::new(file_inputs.len());
        file_inputs
            .into_iter()
            .for_each(|f| files_queue.push(f).unwrap());

        let vecs_pool = FlexiblePool::new(8192);
        let vecs_process_queue = Arc::new(SegQueue::new());

        let mut last_info_log = Mutex::new(Instant::now());

        const MINIMUM_LOG_DELTA_TIME: Duration = Duration::from_secs(15);

        let open_bucket = || {
            let file = files_queue.pop()?;

            let mut last_info_log = last_info_log.lock();
            if last_info_log.elapsed() > MINIMUM_LOG_DELTA_TIME {
                println!(
                    "Processing bucket {} of {} {}",
                    buckets_count - files_queue.len(),
                    buckets_count,
                    PHASES_TIMES_MONITOR
                        .read()
                        .get_formatted_counter_without_memory()
                );
                *last_info_log = Instant::now();
            }

            Some(Arc::new(structs::BucketProcessData::<
                F::InputBucketExtraData,
            >::new(
                file,
                vecs_pool.clone(),
                vecs_process_queue.clone(),
            )))
        };

        let current_bucket = RwLock::new(open_bucket());
        let chunked_pool = ChunkedVectorPool::new(BUFFER_CHUNK_SIZE);
        let reading_finished = AtomicBool::new(false);
        const MAX_HASHES_FOR_FLUSH: MemoryDataSize = MemoryDataSize::from_kibioctets(64.0);
        const MAX_TEMP_SEQUENCES_SIZE: MemoryDataSize = MemoryDataSize::from_kibioctets(64.0);

        crossbeam::thread::scope(|s| {
            for _ in 0..min(buckets_count, threads_count) {
                s.spawn(|_| {
                    let mut buckets: Vec<ChunkedVector<u8>> = vec![];
                    let mut executor = F::new(&extra_data);

                    let mut process_pending_reads = |executor: &mut F::ExecutorType<'a>| {
                        while let Some((seqs, memory_ref)) = vecs_process_queue.pop() {
                            executor.maybe_swap_bucket(&extra_data);
                            process_subbucket::process_subbucket::<F>(&extra_data, seqs, executor);
                            drop(memory_ref);
                        }
                    };

                    'outer_loop: loop {
                        if reading_finished.load(Ordering::Relaxed) {
                            process_pending_reads(&mut executor);
                            break 'outer_loop;
                        }

                        buckets.clear();
                        buckets.resize_with(MERGE_BUCKETS_COUNT, || {
                            ChunkedVector::new(chunked_pool.clone())
                        });

                        let mut bucket = match current_bucket.read().clone() {
                            None => continue,
                            Some(val) => val,
                        };
                        let mut cmp_reads =
                            BucketsThreadDispatcher::new(MAX_TEMP_SEQUENCES_SIZE, &bucket.buckets);

                        let mut continue_read = true;

                        while continue_read {
                            process_pending_reads(&mut executor);

                            continue_read = bucket.reader.read_parallel(|read_extra_data, read| {
                                let preprocess_info =
                                    executor.preprocess_bucket(&extra_data, read_extra_data, read);
                                let bases_slice = read.get_compr_slice();

                                let bucket_index = preprocess_info.bucket as usize;

                                let pointer = buckets[bucket_index].ensure_reserve(
                                    10 + bases_slice.len() + preprocess_info.extra_data.max_size(),
                                );

                                encode_varint_flags::<_, _>(
                                    |slice| buckets[bucket_index].push_contiguous_slice(slice),
                                    read.bases_count() as u64,
                                    F::FLAGS_COUNT,
                                    preprocess_info.flags,
                                );
                                buckets[bucket_index].push_contiguous_slice(bases_slice);
                                preprocess_info
                                    .extra_data
                                    .encode(&mut buckets[bucket_index]);

                                cmp_reads.add_element(
                                    preprocess_info.bucket,
                                    &(),
                                    ReadRef {
                                        read_start: pointer,
                                        hash: preprocess_info.hash,
                                    },
                                );
                            });
                            break;
                        }

                        bucket.add_chunks_refs(&mut buckets);
                        cmp_reads.finalize();

                        drop(bucket);

                        let mut writable_bucket = current_bucket.write();
                        if writable_bucket
                            .as_ref()
                            .map(|x| x.reader.is_finished())
                            .unwrap_or(false)
                        {
                            if let Some(bucket) = open_bucket() {
                                *writable_bucket = Some(bucket);
                            } else {
                                writable_bucket.take();
                                reading_finished.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                    executor.finalize(&extra_data);
                });
            }
        });
    }
}