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

use std::collections::HashMap;
use std::sync::Arc;

use databend_common_catalog::table_context::TableContext;
use databend_common_exception::Result;
use databend_common_expression::FunctionContext;

use crate::executor::explain::PlanStatsInfo;
use crate::executor::PhysicalPlan;
use crate::optimizer::ColumnSet;
use crate::optimizer::RelExpr;
use crate::optimizer::SExpr;
use crate::plans::RelOperator;
use crate::ColumnBinding;
use crate::IndexType;
use crate::MetadataRef;

pub struct PhysicalPlanBuilder {
    pub(crate) metadata: MetadataRef,
    pub(crate) ctx: Arc<dyn TableContext>,
    pub(crate) func_ctx: FunctionContext,
    pub(crate) dry_run: bool,
    // Record cte_idx and the cte's output columns
    pub(crate) cte_output_columns: HashMap<IndexType, Vec<ColumnBinding>>,
}

impl PhysicalPlanBuilder {
    pub fn new(metadata: MetadataRef, ctx: Arc<dyn TableContext>, dry_run: bool) -> Self {
        let func_ctx = ctx.get_function_context().unwrap();
        Self {
            metadata,
            ctx,
            func_ctx,
            dry_run,
            cte_output_columns: Default::default(),
        }
    }

    pub(crate) fn build_plan_stat_info(&self, s_expr: &SExpr) -> Result<PlanStatsInfo> {
        let rel_expr = RelExpr::with_s_expr(s_expr);
        let stat_info = rel_expr.derive_cardinality()?;

        Ok(PlanStatsInfo {
            estimated_rows: stat_info.cardinality,
        })
    }

    pub async fn build(&mut self, s_expr: &SExpr, required: ColumnSet) -> Result<PhysicalPlan> {
        let mut plan = self.build_physical_plan(s_expr, required).await?;
        adjust_plan_id(&mut plan, &mut 0);

        Ok(plan)
    }

    #[async_recursion::async_recursion]
    #[async_backtrace::framed]
    pub async fn build_physical_plan(
        &mut self,
        s_expr: &SExpr,
        required: ColumnSet,
    ) -> Result<PhysicalPlan> {
        // Build stat info.
        let stat_info = self.build_plan_stat_info(s_expr)?;
        match s_expr.plan() {
            RelOperator::Scan(scan) => self.build_table_scan(scan, required, stat_info).await,
            RelOperator::DummyTableScan(_) => self.build_dummy_table_scan().await,
            RelOperator::Join(join) => self.build_join(s_expr, join, required, stat_info).await,
            RelOperator::EvalScalar(eval_scalar) => {
                self.build_eval_scalar(s_expr, eval_scalar, required, stat_info)
                    .await
            }
            RelOperator::Filter(filter) => {
                self.build_filter(s_expr, filter, required, stat_info).await
            }
            RelOperator::Aggregate(agg) => {
                self.build_aggregate(s_expr, agg, required, stat_info).await
            }
            RelOperator::Window(window) => {
                self.build_window(s_expr, window, required, stat_info).await
            }
            RelOperator::Sort(sort) => self.build_sort(s_expr, sort, required, stat_info).await,
            RelOperator::Limit(limit) => self.build_limit(s_expr, limit, required, stat_info).await,
            RelOperator::Exchange(exchange) => {
                self.build_exchange(s_expr, exchange, required).await
            }
            RelOperator::UnionAll(union_all) => {
                self.build_union_all(s_expr, union_all, required, stat_info)
                    .await
            }
            RelOperator::ProjectSet(project_set) => {
                self.build_project_set(s_expr, project_set, required, stat_info)
                    .await
            }
            RelOperator::CteScan(cte_scan) => self.build_cte_scan(cte_scan, required).await,
            RelOperator::MaterializedCte(cte) => {
                self.build_materialized_cte(s_expr, cte, required).await
            }
            RelOperator::ConstantTableScan(scan) => {
                self.build_constant_table_scan(scan, required).await
            }
            RelOperator::AddRowNumber(_) => self.build_add_row_number(s_expr, required).await,
            RelOperator::Udf(udf) => self.build_udf(s_expr, udf, required, stat_info).await,
        }
    }
}

/// Adjust the plan_id of the physical plan.
/// This function will assign a unique plan_id to each physical plan node in a top-down manner.
/// Which means the plan_id of a node is always greater than the plan_id of its parent node.
fn adjust_plan_id(plan: &mut PhysicalPlan, next_id: &mut u32) {
    match plan {
        PhysicalPlan::TableScan(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
        }
        PhysicalPlan::Filter(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::Project(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::EvalScalar(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::ProjectSet(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::AggregateExpand(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::AggregatePartial(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::AggregateFinal(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::Window(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::Sort(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::Limit(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::RowFetch(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::HashJoin(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.probe, next_id);
            adjust_plan_id(&mut plan.build, next_id);
        }
        PhysicalPlan::RangeJoin(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.left, next_id);
            adjust_plan_id(&mut plan.right, next_id);
        }
        PhysicalPlan::Exchange(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::UnionAll(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.left, next_id);
            adjust_plan_id(&mut plan.right, next_id);
        }
        PhysicalPlan::CteScan(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
        }
        PhysicalPlan::MaterializedCte(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
        }
        PhysicalPlan::ConstantTableScan(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
        }
        PhysicalPlan::Udf(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::DistributedInsertSelect(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
            adjust_plan_id(&mut plan.input, next_id);
        }
        PhysicalPlan::ExchangeSource(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
        }
        PhysicalPlan::ExchangeSink(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
        }
        PhysicalPlan::CopyIntoTable(plan) => {
            plan.plan_id = *next_id;
            *next_id += 1;
        }
        _ => {}
    }
}
