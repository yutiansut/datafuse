// Copyright 2021 Datafuse Labs
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use ethnum::i256;

use super::partitioned_payload::PartitionedPayload;
use super::payload::Payload;
use super::probe_state::ProbeState;
use crate::types::binary::BinaryColumn;
use crate::types::binary::BinaryColumnBuilder;
use crate::types::decimal::DecimalType;
use crate::types::nullable::NullableColumn;
use crate::types::string::StringColumn;
use crate::types::ArgType;
use crate::types::BooleanType;
use crate::types::DataType;
use crate::types::DateType;
use crate::types::NumberDataType;
use crate::types::NumberType;
use crate::types::TimestampType;
use crate::with_number_mapped_type;
use crate::Column;
use crate::StateAddr;
use crate::BATCH_SIZE;

pub struct PayloadFlushState {
    pub probe_state: ProbeState,
    pub group_columns: Vec<Column>,
    pub aggregate_results: Vec<Column>,
    pub row_count: usize,

    pub flush_partition: usize,
    pub flush_page: usize,
    pub flush_page_row: usize,

    pub addresses: [*const u8; BATCH_SIZE],
    pub state_places: [StateAddr; BATCH_SIZE],
}

impl Default for PayloadFlushState {
    fn default() -> Self {
        PayloadFlushState {
            probe_state: ProbeState::default(),
            group_columns: Vec::new(),
            aggregate_results: Vec::new(),
            row_count: 0,
            flush_partition: 0,
            flush_page: 0,
            flush_page_row: 0,
            addresses: [std::ptr::null::<u8>(); BATCH_SIZE],
            state_places: [StateAddr::new(0); BATCH_SIZE],
        }
    }
}

unsafe impl Send for PayloadFlushState {}
unsafe impl Sync for PayloadFlushState {}

impl PayloadFlushState {
    pub fn clear(&mut self) {
        self.row_count = 0;
        self.flush_partition = 0;
        self.flush_page = 0;
        self.flush_page_row = 0;
    }

    pub fn take_group_columns(&mut self) -> Vec<Column> {
        std::mem::take(&mut self.group_columns)
    }
    pub fn take_aggregate_results(&mut self) -> Vec<Column> {
        std::mem::take(&mut self.aggregate_results)
    }
}

impl PartitionedPayload {
    pub fn flush(&mut self, state: &mut PayloadFlushState) -> bool {
        if state.flush_partition >= self.payloads.len() {
            return false;
        }

        let p = &self.payloads[state.flush_partition];
        if p.flush(state) {
            true
        } else {
            let partition_idx = state.flush_partition + 1;
            state.clear();
            state.flush_partition = partition_idx;
            self.flush(state)
        }
    }
}

impl Payload {
    pub fn flush(&self, state: &mut PayloadFlushState) -> bool {
        if state.flush_page >= self.pages.len() {
            return false;
        }

        let page = &self.pages[state.flush_page];

        if state.flush_page_row >= page.rows {
            state.flush_page += 1;
            state.flush_page_row = 0;
            state.row_count = 0;

            return self.flush(state);
        }

        let end = (state.flush_page_row + BATCH_SIZE).min(page.rows);
        let rows = end - state.flush_page_row;
        state.group_columns.clear();
        state.row_count = rows;
        state.probe_state.row_count = rows;

        for idx in 0..rows {
            state.addresses[idx] = self.data_ptr(page, idx + state.flush_page_row);
            state.probe_state.group_hashes[idx] =
                unsafe { core::ptr::read::<u64>(state.addresses[idx].add(self.hash_offset) as _) };

            if !self.aggrs.is_empty() {
                state.state_places[idx] = unsafe {
                    StateAddr::new(core::ptr::read::<u64>(
                        state.addresses[idx].add(self.state_offset) as _,
                    ) as usize)
                };
            }
        }

        for col_index in 0..self.group_types.len() {
            let col = self.flush_column(col_index, state);
            state.group_columns.push(col);
        }

        state.flush_page_row = end;
        true
    }

    fn flush_column(&self, col_index: usize, state: &mut PayloadFlushState) -> Column {
        let len = state.probe_state.row_count;

        let col_offset = self.group_offsets[col_index];
        let col = match self.group_types[col_index].remove_nullable() {
            DataType::Null => Column::Null { len },
            DataType::EmptyArray => Column::EmptyArray { len },
            DataType::EmptyMap => Column::EmptyMap { len },
            DataType::Boolean => self.flush_type_column::<BooleanType>(col_offset, state),
            DataType::Number(v) => with_number_mapped_type!(|NUM_TYPE| match v {
                NumberDataType::NUM_TYPE =>
                    self.flush_type_column::<NumberType<NUM_TYPE>>(col_offset, state),
            }),
            DataType::Decimal(v) => match v {
                crate::types::DecimalDataType::Decimal128(_) => {
                    self.flush_type_column::<DecimalType<i128>>(col_offset, state)
                }
                crate::types::DecimalDataType::Decimal256(_) => {
                    self.flush_type_column::<DecimalType<i256>>(col_offset, state)
                }
            },
            DataType::Timestamp => self.flush_type_column::<TimestampType>(col_offset, state),
            DataType::Date => self.flush_type_column::<DateType>(col_offset, state),
            DataType::Binary => Column::Binary(self.flush_binary_column(col_offset, state)),
            DataType::String => Column::String(self.flush_string_column(col_offset, state)),
            DataType::Bitmap => Column::Bitmap(self.flush_binary_column(col_offset, state)),
            DataType::Variant => Column::Variant(self.flush_binary_column(col_offset, state)),
            DataType::Geometry => Column::Geometry(self.flush_binary_column(col_offset, state)),
            DataType::Nullable(_) => unreachable!(),
            DataType::Array(_) => todo!(),
            DataType::Map(_) => todo!(),
            DataType::Tuple(_) => todo!(),
            DataType::Generic(_) => unreachable!(),
        };

        let validity_offset = self.validity_offsets[col_index];
        if self.group_types[col_index].is_nullable() {
            let b = self.flush_type_column::<BooleanType>(validity_offset, state);
            let validity = b.into_boolean().unwrap();

            Column::Nullable(Box::new(NullableColumn {
                column: col,
                validity,
            }))
        } else {
            col
        }
    }

    fn flush_type_column<T: ArgType>(
        &self,
        col_offset: usize,
        state: &mut PayloadFlushState,
    ) -> Column {
        let len = state.probe_state.row_count;
        let iter = (0..len).map(|idx| unsafe {
            core::ptr::read::<T::Scalar>(state.addresses[idx].add(col_offset) as _)
        });
        let col = T::column_from_iter(iter, &[]);
        T::upcast_column(col)
    }

    fn flush_binary_column(
        &self,
        col_offset: usize,
        state: &mut PayloadFlushState,
    ) -> BinaryColumn {
        let len = state.probe_state.row_count;
        let mut binary_builder = BinaryColumnBuilder::with_capacity(len, len * 4);

        unsafe {
            for idx in 0..len {
                let str_len =
                    core::ptr::read::<u32>(state.addresses[idx].add(col_offset) as _) as usize;
                let data_address =
                    core::ptr::read::<u64>(state.addresses[idx].add(col_offset + 4) as _) as usize
                        as *const u8;

                let scalar = std::slice::from_raw_parts(data_address, str_len);

                binary_builder.put_slice(scalar);
                binary_builder.commit_row();
            }
        }
        binary_builder.build()
    }

    fn flush_string_column(
        &self,
        col_offset: usize,
        state: &mut PayloadFlushState,
    ) -> StringColumn {
        unsafe { StringColumn::from_binary_unchecked(self.flush_binary_column(col_offset, state)) }
    }
}
