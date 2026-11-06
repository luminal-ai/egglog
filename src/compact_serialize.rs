//! Numeric compiler export. The diagnostic serializer remains independent so
//! differential tests can compare every row, class, cost and representative.
use crate::{util::HashMap, *};
use core_relations::BaseValuePrinter;
use egraph_serialize::compact::{self as out, ClassId, NodeId};
use numeric_id::NumericId;
use ordered_float::NotNan;
use smallvec::SmallVec;

struct SortInfo {
    sort: ArcSort,
    name: out::Op,
    column: egglog_bridge::ColumnTy,
    eq: bool,
    container: bool,
    values: HashMap<Value, usize>,
}
struct ClassInfo {
    sort: usize,
    value: Value,
    order_value: u64,
    members: SmallVec<[u32; 2]>,
    cursor: u32,
    primitive: Option<u32>,
    exported: bool,
    lets: Option<Box<Vec<String>>>,
}
struct Row<'a> {
    function: &'a Function,
    offset: u64,
    raw_offset: u32,
    op: usize,
    output: usize,
    children: std::ops::Range<usize>,
}
#[derive(Clone, Copy)]
enum Origin {
    Function(usize),
    Primitive(usize),
    Dummy(usize),
}
struct Builder<'a> {
    graph: &'a EGraph,
    sorts: Vec<SortInfo>,
    sort_ids: HashMap<String, usize>,
    classes: Vec<ClassInfo>,
    rows: Vec<Row<'a>>,
    args: Vec<usize>,
    origins: Vec<Origin>,
    result: out::EGraph,
    ops: Vec<out::Op>,
    costs: Vec<(f64, bool)>,
    child_data: Vec<NodeId>,
    child_ranges: Vec<std::ops::Range<u32>>,
}
impl<'a> Builder<'a> {
    fn children(&mut self, id: usize, children: impl IntoIterator<Item = NodeId>) {
        let start = u32::try_from(self.child_data.len()).expect("snapshot operand overflow");
        self.child_data.extend(children);
        let end = u32::try_from(self.child_data.len()).expect("snapshot operand overflow");
        if self.child_ranges.len() <= id {
            self.child_ranges.resize(id + 1, 0..0);
        }
        self.child_ranges[id] = start..end;
    }
    fn sort(&mut self, sort: &ArcSort) -> usize {
        if let Some(&id) = self.sort_ids.get(sort.name()) {
            return id;
        }
        let id = self.sorts.len();
        self.sort_ids.insert(sort.name().to_owned(), id);
        self.sorts.push(SortInfo {
            sort: sort.clone(),
            name: sort.name().into(),
            column: sort.column_ty(&self.graph.backend),
            eq: sort.is_eq_sort(),
            container: sort.is_container_sort(),
            values: HashMap::default(),
        });
        id
    }
    fn intern(&mut self, sort: usize, value: Value) -> usize {
        if let Some(&id) = self.sorts[sort].values.get(&value) {
            return id;
        }
        let canonical = self
            .graph
            .backend
            .get_canon_repr(value, self.sorts[sort].column);
        let id = if let Some(&id) = self.sorts[sort].values.get(&canonical) {
            id
        } else {
            let id = self.classes.len();
            self.classes.push(ClassInfo {
                sort,
                value: canonical,
                order_value: decimal_key(canonical.rep() as u64),
                members: SmallVec::new(),
                cursor: 0,
                primitive: None,
                exported: false,
                lets: None,
            });
            self.sorts[sort].values.insert(canonical, id);
            id
        };
        if value != canonical {
            self.sorts[sort].values.insert(value, id);
        }
        id
    }
    fn node_id(&mut self, origin: Origin) -> usize {
        let id = self.origins.len();
        self.origins.push(origin);
        id
    }
    fn value(&mut self, class: usize) -> usize {
        let sort_id = self.classes[class].sort;
        let value = self.classes[class].value;
        let node = if self.sorts[sort_id].eq {
            if self.classes[class].members.is_empty() {
                let id = self.node_id(Origin::Dummy(class));
                self.result.nodes.insert(
                    NodeId::dense(id),
                    out::Node {
                        op: "[...]".into(),
                        children: Default::default(),
                        eclass: ClassId::dense(class),
                        cost: NotNan::new(f64::INFINITY).unwrap(),
                        subsumed: false,
                    },
                );
                self.classes[class].members.push(u32::try_from(id).unwrap());
            }
            let c = &mut self.classes[class];
            c.cursor = (c.cursor + 1) % c.members.len() as u32;
            c.members[c.cursor as usize] as usize
        } else {
            if !self.sorts[sort_id].container {
                if let Some(id) = self.classes[class].primitive {
                    return id as usize;
                }
            }
            let id = if let Some(id) = self.classes[class].primitive {
                id as usize
            } else {
                let id = self.node_id(Origin::Primitive(class));
                self.classes[class].primitive = Some(u32::try_from(id).unwrap());
                id
            };
            let sort = self.sorts[sort_id].sort.clone();
            let container = self.graph.backend.container_values();
            let children: SmallVec<[NodeId; 6]> = sort
                .inner_values(container, value)
                .into_iter()
                .map(|(s, v)| {
                    let s = self.sort(&s);
                    let c = self.intern(s, v);
                    NodeId::dense(self.value(c))
                })
                .collect();
            let op = if sort.is_container_sort() {
                out::Op::from(sort.serialized_name(container, value))
            } else {
                let ty = self
                    .graph
                    .backend
                    .base_values()
                    .get_ty_by_id(sort.value_type().unwrap());
                out::Op::from_debug(BaseValuePrinter {
                    base: self.graph.backend.base_values(),
                    ty,
                    val: value,
                })
            };
            self.children(id, children);
            self.result.nodes.insert(
                NodeId::dense(id),
                out::Node {
                    op: op.into(),
                    children: Default::default(),
                    eclass: ClassId::dense(class),
                    cost: NotNan::new(1.).unwrap(),
                    subsumed: false,
                },
            );
            id
        };
        if !self.classes[class].exported {
            self.classes[class].exported = true;
            let mut extra = std::collections::HashMap::new();
            if let Some(names) = &self.classes[class].lets {
                extra.insert("let".into(), names.join(", "));
            }
            self.result.class_data.insert(
                ClassId::dense(class),
                out::ClassData {
                    typ: Some(self.sorts[self.classes[class].sort].name.clone()),
                    extra: extra.into(),
                    value: Some(value.rep() as u64),
                },
            );
        }
        node
    }
    fn finish(mut self, roots: &[(ArcSort, Value)]) -> out::EGraph {
        let mut timing = std::time::Instant::now();
        self.result.nodes.reserve(self.rows.len());
        self.result.class_data.reserve(self.classes.len());
        for i in 0..self.rows.len() {
            let output = self.rows[i].output;
            self.value(output);
            let mut children = SmallVec::<[NodeId; 6]>::new();
            for a in self.rows[i].children.clone() {
                let class = self.args[a];
                children.push(NodeId::dense(self.value(class)));
            }
            let op = self.ops[self.rows[i].op].clone();
            self.children(i, children);
            self.result.nodes.insert(
                NodeId::dense(i),
                out::Node {
                    op,
                    children: Default::default(),
                    eclass: ClassId::dense(output),
                    cost: NotNan::new(self.costs[i].0).unwrap(),
                    subsumed: self.costs[i].1,
                },
            );
        }
        for (s, v) in roots {
            let s = self.sort(s);
            let c = self.intern(s, *v);
            self.result.root_eclasses.push(ClassId::dense(c));
        }
        trace_time("emit", &mut timing);
        // Existing readers break ties by lexical diagnostic IDs. Assign dense
        // ranks in exactly that order, without allocating diagnostic strings.
        let mut sort_order: Vec<_> = (0..self.sorts.len()).collect();
        sort_order.sort_unstable_by_key(|&s| format!("{}-", self.sorts[s].sort.name()));
        let prefixes: Vec<_> = sort_order
            .iter()
            .map(|&s| format!("{}-", self.sorts[s].sort.name()))
            .collect();
        let ambiguous = prefixes.windows(2).any(|p| p[1].starts_with(&p[0]));
        let class_rank = if ambiguous {
            // Arbitrary user sort names can interleave with decimal values
            // (A-12 versus A-1-2). Retain exact string ordering in that case.
            let mut keys: Vec<_> = self
                .classes
                .iter()
                .enumerate()
                .map(|(id, c)| {
                    (
                        format!("{}-{}", self.sorts[c.sort].sort.name(), c.value.rep()),
                        id,
                    )
                })
                .collect();
            keys.sort_unstable();
            inverse_order(keys.into_iter().map(|(_, id)| id), self.classes.len())
        } else {
            let mut sort_rank = vec![0; sort_order.len()];
            for (i, s) in sort_order.into_iter().enumerate() {
                sort_rank[s] = i;
            }
            assert!(sort_rank.len() < (1 << 24));
            ordered_ranks(
                self.classes
                    .iter()
                    .map(|c| ((sort_rank[c.sort] as u64) << 40) | (c.order_value >> 24))
                    .collect(),
            )
        };
        trace_time("class_order", &mut timing);
        let mut functions: Vec<_> = self.ops.iter().map(|op| op.as_str()).collect();
        functions.sort_unstable();
        functions.dedup();
        let function_rank: HashMap<_, _> = functions
            .into_iter()
            .enumerate()
            .map(|(i, s)| (s, i))
            .collect();
        assert!(function_rank.len() < (1 << 22));
        let node_rank = ordered_ranks(
            self.origins
                .iter()
                .map(|n| match *n {
                    Origin::Dummy(c) => class_rank[c] as u64,
                    Origin::Function(r) => {
                        (1u64 << 62)
                            | ((self.rows[r].offset >> 24) << 22)
                            | function_rank[self.rows[r].function.decl.name.as_str()] as u64
                    }
                    Origin::Primitive(c) => (2u64 << 62) | class_rank[c] as u64,
                })
                .collect(),
        );
        trace_time("node_order", &mut timing);
        let first = self
            .origins
            .iter()
            .filter(|n| matches!(n, Origin::Dummy(_)))
            .count() as u32;
        self.result.function_nodes = Some(first..first + self.rows.len() as u32);
        let mut function_offsets = vec![0; self.rows.len()];
        for (row, info) in self.rows.iter().enumerate() {
            function_offsets[node_rank[row] - first as usize] = info.raw_offset;
        }
        self.result.function_offsets = Some(Arc::new(function_offsets));
        for child in &mut self.child_data {
            *child = NodeId::dense(node_rank[child.index().unwrap()]);
        }
        let children = Arc::new(std::mem::take(&mut self.child_data));
        self.result.nodes.remap_entries(|id, n| {
            n.eclass = ClassId::dense(class_rank[n.eclass.index().unwrap()]);
            n.children = out::Children::shared(
                children.clone(),
                self.child_ranges
                    .get(id.index().unwrap())
                    .cloned()
                    .unwrap_or(0..0),
            );
            *id = NodeId::dense(node_rank[id.index().unwrap()]);
        });
        self.result
            .class_data
            .remap_entries(|id, _| *id = ClassId::dense(class_rank[id.index().unwrap()]));
        for id in &mut self.result.root_eclasses {
            *id = ClassId::dense(class_rank[id.index().unwrap()]);
        }
        trace_time("remap", &mut timing);
        self.result
    }
}

fn inverse_order(order: impl Iterator<Item = usize>, len: usize) -> Vec<usize> {
    let mut rank = vec![0; len];
    for (i, id) in order.enumerate() {
        rank[id] = i;
    }
    rank
}

/// Cache-friendly integer radix order. Six linear passes replace millions of
/// random class/string comparisons. Keys are unique legacy lexical ranks.
fn ordered_ranks(keys: Vec<u64>) -> Vec<usize> {
    let len = keys.len();
    let mut rows: Vec<_> = keys
        .into_iter()
        .enumerate()
        .map(|(id, key)| (key, id))
        .collect();
    if len < 4096 {
        rows.sort_unstable_by_key(|r| r.0);
    } else {
        let mut scratch = vec![(0, 0); len];
        for shift in (0..64).step_by(11) {
            let mut counts = [0usize; 2048];
            for &(key, _) in &rows {
                counts[((key >> shift) & 2047) as usize] += 1;
            }
            let mut next = 0;
            for count in &mut counts {
                let n = *count;
                *count = next;
                next += n;
            }
            for &row in &rows {
                let bucket = ((row.0 >> shift) & 2047) as usize;
                scratch[counts[bucket]] = row;
                counts[bucket] += 1;
            }
            std::mem::swap(&mut rows, &mut scratch);
        }
    }
    inverse_order(rows.into_iter().map(|(_, id)| id), len)
}

fn trace_time(stage: &str, start: &mut std::time::Instant) {
    if std::env::var_os("EGGLOG_SNAPSHOT_TIMING").is_some() {
        eprintln!(
            "COMPACT stage={stage} seconds={:.6}",
            start.elapsed().as_secs_f64()
        );
    }
    *start = std::time::Instant::now();
}

// Ten decimal digits fit in forty bits. Digit+1 leaves a zero terminator,
// so the fixed-width unsigned key has exactly decimal lexical ordering.
fn decimal_key(n: u64) -> u64 {
    let mut buffer = [0u8; 20];
    let mut at = 20;
    let mut value = n;
    loop {
        at -= 1;
        buffer[at] = (value % 10) as u8 + 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    assert!(
        20 - at <= 16,
        "snapshot numeric ID exceeds lexical key width"
    );
    buffer[at..]
        .iter()
        .enumerate()
        .fold(0, |key, (i, d)| key | ((*d as u64) << (60 - i * 4)))
}
impl EGraph {
    fn compact_stamp(
        &self,
        snapshot: &out::EGraph,
        include: impl Fn(&str) -> bool,
    ) -> out::SourceStamp {
        out::SourceStamp {
            identity: self.backend.snapshot_identity(),
            tables: self
                .functions
                .iter()
                .filter(|(name, f)| f.decl.internal_let || include(name))
                .map(|(name, f)| {
                    (
                        name.clone(),
                        self.backend.snapshot_table_version(f.backend_id),
                    )
                })
                .collect(),
            node_revision: snapshot.nodes.revision(),
            class_revision: snapshot.class_data.revision(),
            function_nodes: snapshot.function_nodes.clone(),
        }
    }

    /// A table stamp proves unchanged rows, not just unchanged table length.
    /// Validate every changed table by canonical row identity before publishing
    /// anything. A different database/union epoch or edited snapshot falls back
    /// to full validation. Archives intentionally carry no such proof.
    fn refresh_compact_delta(
        &self,
        snapshot: &mut out::EGraph,
        omit: &[&str],
        include: &impl Fn(&str) -> bool,
    ) -> Option<bool> {
        let stamp = snapshot.source.as_ref()?;
        if stamp.identity != self.backend.snapshot_identity()
            || stamp.node_revision != snapshot.nodes.revision()
            || stamp.class_revision != snapshot.class_data.revision()
            || stamp.function_nodes != snapshot.function_nodes
        {
            return None;
        }
        let range = snapshot.function_nodes.as_ref()?;
        let mut next = self.compact_stamp(snapshot, include);
        if stamp
            .tables
            .keys()
            .any(|name| !next.tables.contains_key(name))
        {
            return Some(false);
        }
        let mut updates = Vec::new();
        for (name, function) in &self.functions {
            if !function.decl.internal_let && !include(name) {
                continue;
            }
            let current = next.tables[name];
            let previous = stamp.tables.get(name).copied();
            if previous == Some(current) {
                continue;
            }
            if function.decl.internal_let {
                return Some(false);
            }
            if previous.map_or(0, |v| v.3) == 0 && current.3 == 0 {
                continue;
            }
            if previous.map_or(0, |v| v.3) == 0 && omit.contains(&name.as_str()) {
                // Keep proving this omission on every call, including when a
                // later caller no longer permits this annotation relation.
                if let Some(old) = previous {
                    next.tables.insert(name.clone(), old);
                } else {
                    next.tables.shift_remove(name);
                }
                continue;
            }
            let ids = snapshot
                .indexes()
                .operators
                .get(name.as_str())
                .into_iter()
                .flatten()
                .filter(|id| id.index().is_some_and(|i| range.contains(&(i as u32))));
            let mut rows: HashMap<SmallVec<[u64; 5]>, &NodeId> = HashMap::default();
            for id in ids {
                let node = &snapshot.nodes[id];
                if node.children.len() != function.func_type.input.len() {
                    return Some(false);
                }
                let key: Option<SmallVec<[u64; 5]>> = node
                    .children
                    .iter()
                    .map(|id| &snapshot.nodes[id].eclass)
                    .chain(std::iter::once(&node.eclass))
                    .map(|c| snapshot.class_data[c].value)
                    .collect();
                if rows.insert(key?, id).is_some() {
                    return Some(false);
                }
            }
            let mut matches = true;
            self.backend.for_each_while(function.backend_id, |row| {
                let key: SmallVec<[u64; 5]> = row
                    .vals
                    .iter()
                    .zip(
                        function
                            .func_type
                            .input
                            .iter()
                            .chain(std::iter::once(&function.func_type.output)),
                    )
                    .map(|(&v, s)| {
                        self.backend
                            .get_canon_repr(v, s.column_ty(&self.backend))
                            .rep() as u64
                    })
                    .collect();
                let Some(id) = rows.remove(&key) else {
                    matches = false;
                    return false;
                };
                if snapshot.nodes[id].subsumed != row.subsumed {
                    updates.push((id.clone(), row.subsumed));
                }
                true
            });
            if !matches || !rows.is_empty() {
                return Some(false);
            }
        }
        for (id, subsumed) in updates {
            snapshot.nodes.get_mut(&id).unwrap().subsumed = subsumed;
        }
        next.node_revision = snapshot.nodes.revision();
        snapshot.source = Some(next);
        Some(true)
    }

    /// Validate all retained numeric rows before publishing any flag changes.
    /// Table compaction may reorder rows; argument/output identities, rather
    /// than table offsets, identify them. Structural edits fail closed.
    pub fn refresh_compact_subsumption(
        &self,
        snapshot: &mut out::EGraph,
        omit: &[&str],
        include: impl Fn(&str) -> bool,
    ) -> bool {
        if let Some(result) = self.refresh_compact_delta(snapshot, omit, &include) {
            return result;
        }
        let Some(range) = &snapshot.function_nodes else {
            return false;
        };
        let mut calls: HashMap<&str, Vec<&NodeId>> = HashMap::default();
        for (id, node) in &snapshot.nodes {
            if id.index().is_some_and(|i| range.contains(&(i as u32))) {
                calls.entry(node.op.as_str()).or_default().push(id);
            }
        }
        let mut globals = HashMap::default();
        let mut classes = HashMap::default();
        for (id, data) in &snapshot.class_data {
            let (Some(sort), Some(value)) = (&data.typ, data.value) else {
                return false;
            };
            classes.insert((sort.as_str(), value), id);
            if let Some(names) = data.extra.get("let") {
                for name in names.split(", ") {
                    globals.insert(name, id);
                }
            }
        }
        let mut updates = Vec::new();
        for (name, function) in &self.functions {
            if !function.decl.internal_let && !include(name) {
                continue;
            }
            if function.decl.internal_let {
                let previous = globals.remove(name.as_str());
                let mut count = 0;
                let mut matches = true;
                self.backend.for_each_while(function.backend_id, |row| {
                    count += 1;
                    let sort = &function.func_type.output;
                    let value = self
                        .backend
                        .get_canon_repr(*row.vals.last().unwrap(), sort.column_ty(&self.backend))
                        .rep() as u64;
                    let current = classes.get(&(sort.name(), value)).copied();
                    matches = previous == current;
                    matches
                });
                if !matches || count > 1 || (count == 0 && previous.is_some()) {
                    return false;
                }
                continue;
            }
            let previous = calls.remove(name.as_str()).unwrap_or_default();
            if previous.is_empty() && omit.contains(&name.as_str()) {
                continue;
            }
            let mut rows: HashMap<SmallVec<[u64; 5]>, &NodeId> = HashMap::default();
            for id in previous {
                let node = &snapshot.nodes[id];
                if node.children.len() != function.func_type.input.len() {
                    return false;
                }
                let mut key = SmallVec::new();
                for (class, sort) in node
                    .children
                    .iter()
                    .map(|id| &snapshot.nodes[id].eclass)
                    .chain(std::iter::once(&node.eclass))
                    .zip(
                        function
                            .func_type
                            .input
                            .iter()
                            .chain(std::iter::once(&function.func_type.output)),
                    )
                {
                    let data = &snapshot.class_data[class];
                    if data.typ.as_deref() != Some(sort.name()) {
                        return false;
                    }
                    let Some(value) = data.value else {
                        return false;
                    };
                    key.push(value);
                }
                if rows.insert(key, id).is_some() {
                    return false;
                }
            }
            let mut matches = true;
            self.backend.for_each_while(function.backend_id, |row| {
                let key: SmallVec<[u64; 5]> = row
                    .vals
                    .iter()
                    .zip(
                        function
                            .func_type
                            .input
                            .iter()
                            .chain(std::iter::once(&function.func_type.output)),
                    )
                    .map(|(&v, s)| {
                        self.backend
                            .get_canon_repr(v, s.column_ty(&self.backend))
                            .rep() as u64
                    })
                    .collect();
                let Some(id) = rows.remove(&key) else {
                    matches = false;
                    return false;
                };
                if snapshot.nodes[id].subsumed != row.subsumed {
                    updates.push((id.clone(), row.subsumed));
                }
                true
            });
            if !matches || !rows.is_empty() {
                return false;
            }
        }
        if !calls.is_empty() || !globals.is_empty() {
            return false;
        }
        drop(calls);
        drop(globals);
        drop(classes);
        for (id, subsumed) in updates {
            snapshot.nodes.get_mut(&id).unwrap().subsumed = subsumed;
        }
        true
    }

    /// Complete compiler snapshot of selected functions. Globals and container
    /// contents are retained; an omitted class remains an explicit `[...]`.
    pub fn serialize_compact_filtered(&self, include: impl Fn(&str) -> bool) -> out::EGraph {
        let mut timing = std::time::Instant::now();
        let mut b = Builder {
            graph: self,
            sorts: Vec::new(),
            sort_ids: HashMap::default(),
            classes: Vec::new(),
            rows: Vec::new(),
            args: Vec::new(),
            origins: Vec::new(),
            result: out::EGraph::default(),
            ops: Vec::new(),
            costs: Vec::new(),
            child_data: Vec::new(),
            child_ranges: Vec::new(),
        };
        let mut total_rows = 0;
        let mut total_args = 0;
        let mut per_sort = HashMap::default();
        for (name, function) in &self.functions {
            if !function.decl.internal_let && !include(name) {
                continue;
            }
            let count = self.backend.table_size(function.backend_id);
            if !function.decl.internal_let {
                total_rows += count;
                total_args += count * function.func_type.input.len();
            }
            for sort in function
                .func_type
                .input
                .iter()
                .chain(std::iter::once(&function.func_type.output))
            {
                let id = b.sort(sort);
                let max = per_sort.entry(id).or_insert(0);
                let reserve =
                    if sort.is_eq_sort() && sort.name() == function.func_type.output.name() {
                        count
                    } else {
                        count.min(65536)
                    };
                *max = (*max).max(reserve);
            }
        }
        b.rows.reserve(total_rows);
        b.origins.reserve(total_rows);
        b.costs.reserve(total_rows);
        b.args.reserve(total_args);
        b.classes.reserve(total_rows);
        for (id, count) in per_sort {
            b.sorts[id].values.reserve(count);
        }
        for (name, function) in &self.functions {
            if !function.decl.internal_let && !include(name) {
                continue;
            }
            let op = b.ops.len();
            b.ops.push(name.clone().into());
            let output = b.sort(&function.func_type.output);
            let inputs: Vec<_> = function.func_type.input.iter().map(|s| b.sort(s)).collect();
            let mut offset = 0;
            self.backend.for_each_while(function.backend_id, |row| {
                let (out, values) = row.vals.split_last().unwrap();
                let class = b.intern(output, *out);
                if function.decl.internal_let {
                    b.classes[class]
                        .lets
                        .get_or_insert_with(Default::default)
                        .push(name.clone());
                } else {
                    let index = b.rows.len();
                    let start = b.args.len();
                    for (&s, &v) in inputs.iter().zip(values) {
                        let c = b.intern(s, v);
                        b.args.push(c);
                    }
                    let end = b.args.len();
                    b.rows.push(Row {
                        function,
                        offset: decimal_key(offset),
                        raw_offset: u32::try_from(offset).unwrap(),
                        op,
                        output: class,
                        children: start..end,
                    });
                    b.costs
                        .push((function.decl.cost.unwrap_or(1) as f64, row.subsumed));
                    b.origins.push(Origin::Function(index));
                    if function.func_type.output.is_eq_sort() {
                        b.classes[class].members.push(u32::try_from(index).unwrap());
                    }
                    offset += 1;
                }
                true
            });
        }
        trace_time("scan", &mut timing);
        let mut result = b.finish(&[]);
        result.source = Some(self.compact_stamp(&result, include));
        result
    }
}
