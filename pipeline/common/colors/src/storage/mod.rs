use crate::storage::serializer::ColorsFlushProcessing;
use config::ColorIndexType;
use std::io::Read;

pub mod deserializer;
pub mod roaring;
pub mod run_length;
pub mod serializer;

pub trait ColorsSerializerTrait {
    const MAGIC: [u8; 16];

    fn decode_color(reader: impl Read, out_vec: Option<&mut Vec<ColorIndexType>>);
    // fn decode_colors(reader: impl Read) -> ;

    fn new(writer: ColorsFlushProcessing, checkpoint_distance: usize, colors_count: u64) -> Self;
    fn serialize_colors(&self, colors: &[ColorIndexType]) -> ColorIndexType;
    fn get_subsets_count(&self) -> u64;
    fn print_stats(&self);
    fn finalize(self) -> ColorsFlushProcessing;
}
