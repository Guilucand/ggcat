use io::sequences_reader::FastaSequence;
use parallel_processor::execution_manager::objects_pool::PoolObjectTrait;
use parallel_processor::execution_manager::packet::PacketTrait;
use std::mem::size_of;

type SequencesType = (usize, usize, usize, usize);

pub struct MinimizerBucketingQueueData<F: Clone + Sync + Send + Default + 'static> {
    data: Vec<u8>,
    pub sequences: Vec<SequencesType>,
    pub file_info: F,
    pub start_read_index: u64,
}

impl<F: Clone + Sync + Send + Default + 'static> MinimizerBucketingQueueData<F> {
    pub fn new(capacity: usize, file_info: F) -> Self {
        Self {
            data: Vec::with_capacity(capacity),
            sequences: Vec::with_capacity(capacity / 512),
            file_info,
            start_read_index: 0,
        }
    }

    pub fn push_sequences(&mut self, seq: FastaSequence) -> bool {
        let qual_len = seq.qual.map(|q| q.len()).unwrap_or(0);
        let ident_len = seq.ident.len();
        let seq_len = seq.seq.len();

        let tot_len = qual_len + ident_len + seq_len;

        if self.data.len() != 0 && (self.data.capacity() - self.data.len()) < tot_len {
            return false;
        }

        let start = self.data.len();
        self.data.extend_from_slice(seq.ident);
        self.data.extend_from_slice(seq.seq);
        if let Some(qual) = seq.qual {
            self.data.extend_from_slice(qual);
        }

        self.sequences.push((start, ident_len, seq_len, qual_len));

        true
    }

    pub fn iter_sequences(&self) -> impl Iterator<Item = FastaSequence> {
        self.sequences
            .iter()
            .map(move |&(start, id_len, seq_len, qual_len)| {
                let mut start = start;

                let ident = &self.data[start..start + id_len];
                start += id_len;

                let seq = &self.data[start..start + seq_len];
                start += seq_len;

                let qual = match qual_len {
                    0 => None,
                    _ => Some(&self.data[start..start + qual_len]),
                };

                FastaSequence { ident, seq, qual }
            })
    }
}

impl<F: Clone + Sync + Send + Default + 'static> PoolObjectTrait
    for MinimizerBucketingQueueData<F>
{
    type InitData = usize;

    fn allocate_new(init_data: &Self::InitData) -> Self {
        Self::new(*init_data, F::default())
    }

    fn reset(&mut self) {
        self.data.clear();
        self.sequences.clear();
    }
}

impl<F: Clone + Sync + Send + Default + 'static> PacketTrait for MinimizerBucketingQueueData<F> {
    fn get_size(&self) -> usize {
        self.data.len() + (self.sequences.len() * size_of::<SequencesType>())
    }
}
