//! Read-only access for external snapshot formats. No snapshot representation
//! or compiler-specific storage policy belongs to this interface.
use crate::{ArcSort, EGraph, Function, Value};
use core_relations::BaseValuePrinter;
use egglog_numeric_id::NumericId;
use egglog_core_relations as core_relations;

pub struct SnapshotReader<'a>(&'a EGraph);

pub struct SnapshotRow<'a> {
    pub vals: &'a [Value],
    pub subsumed: bool,
}

pub fn value_bits(value: Value) -> u64 {
    value.rep() as u64
}

impl EGraph {
    pub fn snapshot_reader(&self) -> SnapshotReader<'_> {
        SnapshotReader(self)
    }
}

impl Function {
    pub fn cost(&self) -> Option<u64> {
        self.decl.cost
    }
}

impl SnapshotReader<'_> {
    pub fn identity(&self) -> (u64, usize) {
        self.0.backend.snapshot_identity()
    }

    pub fn table_version(&self, function: &Function) -> (usize, usize, usize, usize) {
        self.0.backend.snapshot_table_version(function.backend_id)
    }

    pub fn table_size(&self, function: &Function) -> usize {
        self.0.backend.table_size(function.backend_id)
    }

    pub fn canonical_value(&self, value: Value, sort: &ArcSort) -> Value {
        self.0.backend.get_canon_repr(value, sort.column_ty(&self.0.backend))
    }

    pub fn for_each_while(&self, function: &Function, mut visit: impl FnMut(SnapshotRow<'_>) -> bool) {
        self.0.backend.for_each_while(function.backend_id, |row| {
            visit(SnapshotRow { vals: row.vals, subsumed: row.subsumed })
        });
    }

    pub fn inner_values(&self, sort: &ArcSort, value: Value) -> Vec<(ArcSort, Value)> {
        sort.inner_values(self.0.backend.container_values(), value)
    }

    pub fn container_name(&self, sort: &ArcSort, value: Value) -> String {
        sort.serialized_name(self.0.backend.container_values(), value)
    }

    pub fn primitive_name(&self, sort: &ArcSort, value: Value) -> String {
        let base = self.0.backend.base_values();
        format!("{:?}", BaseValuePrinter {
            base,
            ty: base.get_ty_by_id(sort.value_type().expect("primitive sort")),
            val: value,
        })
    }
}
