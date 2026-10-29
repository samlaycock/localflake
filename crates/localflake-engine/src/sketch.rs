//! States for Snowflake's sketch functions: HyperLogLog (`HLL_ACCUMULATE`),
//! bitmaps (`BITMAP_CONSTRUCT_AGG`) and MinHash (`MINHASH`).
//!
//! The translator collects each group's values with DuckDB's `list` aggregate
//! and these functions turn the lists into states and back. States are exact
//! rather than approximate, so estimates are exact counts locally:
//!
//! - HyperLogLog: BINARY, a marker then the distinct 64-bit value hashes
//!   (little-endian, sorted).
//! - Bitmaps: BINARY in Snowflake's documented index-vector layout: the bit
//!   count (big-endian u16) then each set bit's position (little-endian u16),
//!   padded to 10 bytes. `0001 0000 000000000000` is the bitmap with bit 0 set.
//! - MinHash: the minimum of `k` seeded hashes of the values.

use std::error::Error;
use std::sync::Arc;

use duckdb::arrow::array::{Array, ArrayRef, AsArray, BinaryArray, ListArray, RecordBatch};
use duckdb::arrow::datatypes::{DataType, Field, Int64Type, UInt64Type};
use duckdb::vscalar::{ArrowFunctionSignature, VArrowScalar};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const HLL_MARKER: &[u8] = b"LFHLL1";
const BITMAP_MIN_BYTES: usize = 10;
/// MinHash values stay below 2^53, so they survive JSON in every driver.
const MINHASH_MASK: u64 = (1 << 53) - 1;

fn list_of(item: DataType) -> DataType {
    DataType::List(Arc::new(Field::new("item", item, true)))
}

/// The non-NULL values of row `row` of a list column.
fn row_values<T: duckdb::arrow::datatypes::ArrowPrimitiveType>(lists: &ListArray, row: usize) -> Vec<T::Native> {
    let values = lists.value(row);
    let values = values.as_primitive::<T>();
    values.iter().flatten().collect()
}

/// `sf_hll_state(hashes UBIGINT[]) -> BLOB`.
pub struct HllState;

impl VArrowScalar for HllState {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let lists = input.column(0).as_list::<i32>();
        let out: BinaryArray = (0..lists.len())
            .map(|row| {
                (!lists.is_null(row)).then(|| {
                    let mut hashes = row_values::<UInt64Type>(lists, row);
                    hashes.sort_unstable();
                    hashes.dedup();
                    let mut state = HLL_MARKER.to_vec();
                    hashes.iter().for_each(|h| state.extend_from_slice(&h.to_le_bytes()));
                    state
                })
            })
            .collect();
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![list_of(DataType::UInt64)], DataType::Binary)]
    }
}

/// `sf_hll_hashes(state BLOB) -> UBIGINT[]`.
pub struct HllHashes;

impl VArrowScalar for HllHashes {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let states = input.column(0).as_binary::<i32>();
        let rows = (0..states.len())
            .map(|row| -> Result<Option<Vec<Option<u64>>>> {
                if states.is_null(row) {
                    return Ok(None);
                }
                let body = states.value(row).strip_prefix(HLL_MARKER).ok_or("not a HyperLogLog state")?;
                Ok(Some(body.as_chunks::<8>().0.iter().map(|c| Some(u64::from_le_bytes(*c))).collect()))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Arc::new(ListArray::from_iter_primitive::<UInt64Type, _, _>(rows)))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Binary], list_of(DataType::UInt64))]
    }
}

/// `sf_bitmap_state(positions BIGINT[]) -> BLOB`.
pub struct BitmapState;

impl VArrowScalar for BitmapState {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let lists = input.column(0).as_list::<i32>();
        let out = (0..lists.len())
            .map(|row| -> Result<Option<Vec<u8>>> {
                if lists.is_null(row) {
                    return Ok(None);
                }
                let mut positions = row_values::<Int64Type>(lists, row);
                positions.sort_unstable();
                positions.dedup();
                if let Some(bad) = positions.iter().find(|p| !(0..32_768).contains(*p)) {
                    return Err(format!("bit position {bad} is outside a bitmap (0 to 32767)").into());
                }
                let mut state = (positions.len() as u16).to_be_bytes().to_vec();
                positions.iter().for_each(|p| state.extend_from_slice(&(*p as u16).to_le_bytes()));
                state.resize(state.len().max(BITMAP_MIN_BYTES), 0);
                Ok(Some(state))
            })
            .collect::<Result<BinaryArray>>()?;
        Ok(Arc::new(out))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![list_of(DataType::Int64)], DataType::Binary)]
    }
}

/// `sf_bitmap_positions(bitmap BLOB) -> BIGINT[]`.
pub struct BitmapPositions;

impl VArrowScalar for BitmapPositions {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let bitmaps = input.column(0).as_binary::<i32>();
        let rows = (0..bitmaps.len())
            .map(|row| -> Result<Option<Vec<Option<i64>>>> {
                if bitmaps.is_null(row) {
                    return Ok(None);
                }
                let bytes = bitmaps.value(row);
                let count = bytes.get(..2).map(|c| u16::from_be_bytes([c[0], c[1]]) as usize).ok_or("not a bitmap")?;
                let positions = bytes.get(2..2 + count * 2).ok_or("not a bitmap")?;
                Ok(Some(positions.as_chunks::<2>().0.iter().map(|c| Some(u16::from_le_bytes(*c) as i64)).collect()))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Arc::new(ListArray::from_iter_primitive::<Int64Type, _, _>(rows)))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![DataType::Binary], list_of(DataType::Int64))]
    }
}

/// `sf_minhash(hashes UBIGINT[], k BIGINT) -> BIGINT[]`: for each of `k` seeds,
/// the minimum seeded hash of the values.
pub struct MinHash;

impl VArrowScalar for MinHash {
    type State = ();

    fn invoke(_: &(), input: RecordBatch) -> Result<ArrayRef> {
        let lists = input.column(0).as_list::<i32>();
        let ks = input.column(1).as_primitive::<Int64Type>();
        let rows = (0..lists.len())
            .map(|row| -> Result<Option<Vec<Option<i64>>>> {
                if lists.is_null(row) || ks.is_null(row) {
                    return Ok(None);
                }
                let k = ks.value(row);
                if !(1..=1024).contains(&k) {
                    return Err(format!("MINHASH: the number of hash functions must be 1 to 1024, not {k}").into());
                }
                let hashes = row_values::<UInt64Type>(lists, row);
                Ok(Some(
                    (0..k as u64)
                        .map(|seed| {
                            let min = hashes.iter().map(|h| splitmix(h ^ splitmix(seed)) & MINHASH_MASK).min();
                            Some(min.unwrap_or(MINHASH_MASK) as i64)
                        })
                        .collect(),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Arc::new(ListArray::from_iter_primitive::<Int64Type, _, _>(rows)))
    }

    fn signatures() -> Vec<ArrowFunctionSignature> {
        vec![ArrowFunctionSignature::exact(vec![list_of(DataType::UInt64), DataType::Int64], list_of(DataType::Int64))]
    }
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests {
    #[test]
    fn splitmix_spreads_seeds() {
        assert_ne!(super::splitmix(0), super::splitmix(1));
    }
}
