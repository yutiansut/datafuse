//  Copyright 2021 Datafuse Labs.
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
//

use common_base::BlockingWait;
use common_datavalues::DataSchemaRef;
use common_exception::Result;
use common_planners::Extras;

use crate::datasources::table::fuse::index::MinMaxIndex;
use crate::datasources::table::fuse::BlockMeta;
use crate::datasources::table::fuse::MetaInfoReader;
use crate::datasources::table::fuse::TableSnapshot;

pub fn range_filter(
    table_snapshot: &TableSnapshot,
    schema: DataSchemaRef,
    push_down: Option<Extras>,
    meta_reader: MetaInfoReader,
) -> Result<Vec<BlockMeta>> {
    let range_index = MinMaxIndex::new(table_snapshot, &meta_reader);
    async move { range_index.apply(schema, push_down).await }
        .wait_in(meta_reader.runtime(), None)?
}
