use crate::{util::HashMap, *};
use core_relations::BaseValuePrinter;
use ordered_float::NotNan;
use smallvec::SmallVec;

pub struct SerializeConfig {
    // Maximumum number of functions to include in the serialized graph, any after this will be discarded
    pub max_functions: Option<usize>,
    // Maximum number of calls to include per function, any after this will be discarded
    pub max_calls_per_function: Option<usize>,
    // Whether to include temporary functions in the serialized graph
    pub include_temporary_functions: bool,
    // Root eclasses to include in the output
    pub root_eclasses: Vec<(ArcSort, Value)>,
}

/// Output of serializing an e-graph, including values that were omitted if any.
pub struct SerializeOutput {
    /// The serialized e-graph.
    pub egraph: egraph_serialize::EGraph,
    /// Functions with more calls than max_calls_per_function, so that not all values are included.
    pub truncated_functions: Vec<String>,
    /// Functions that were discarded from the output, because more functions were present than max_functions
    pub discarded_functions: Vec<String>,
}

impl SerializeOutput {
    /// Returns true if the serialization is complete and no functions were truncated or discarded.
    pub fn is_complete(&self) -> bool {
        self.truncated_functions.is_empty() && self.discarded_functions.is_empty()
    }
    /// Description of what was omitted from the e-graph
    pub fn omitted_description(&self) -> String {
        let mut msg = String::new();
        if !self.discarded_functions.is_empty() {
            msg.push_str(&format!(
                "Omitted: {}\n",
                self.discarded_functions.join(", ")
            ));
        }
        if !self.truncated_functions.is_empty() {
            msg.push_str(&format!(
                "Truncated: {}\n",
                self.truncated_functions.join(", ")
            ));
        }
        msg
    }
}

#[allow(dead_code)]
struct Serializer {
    node_ids: NodeIDs,
    result: egraph_serialize::EGraph,
    let_bindings: HashMap<egraph_serialize::ClassId, Vec<String>>,
    class_ids: ClassIDs,
    primitive_ids: HashMap<egraph_serialize::ClassId, egraph_serialize::NodeId>,
}

/// Valid only for one immutable snapshot. Sort names, not temporary ArcSort
/// addresses, identify sorts returned by container traversal.
#[derive(Default)]
struct ClassIDs(HashMap<String, HashMap<Value, egraph_serialize::ClassId>>);

impl ClassIDs {
    fn get(&mut self, graph: &EGraph, sort: &ArcSort, value: Value) -> egraph_serialize::ClassId {
        let values = self.0.entry_ref(sort.name()).or_default();
        values
            .entry(value)
            .or_insert_with(|| graph.value_to_class_id(sort, value))
            .clone()
    }
}

/// Default is used for exporting JSON and will output all nodes.
impl Default for SerializeConfig {
    fn default() -> Self {
        SerializeConfig {
            max_functions: None,
            max_calls_per_function: None,
            include_temporary_functions: false,
            root_eclasses: vec![],
        }
    }
}

/// A node in the serialized egraph.
#[derive(PartialEq, Debug, Clone)]
pub enum SerializedNode {
    /// A user defined function call.
    Function {
        /// The name of the function.
        name: String,
        /// The offset of the index in the table.
        /// This can be resolved to the output and input values with table.get_index(offset, true).
        offset: usize,
    },
    /// A primitive value.
    Primitive(Value),
    /// A dummy node used to represent omitted nodes.
    Dummy(Value),
    /// A node that was split into multiple e-classes.
    Split(Box<SerializedNode>),
}

impl SerializedNode {
    /// Returns true if the node is a primitive value.
    pub fn is_primitive(&self) -> bool {
        match self {
            SerializedNode::Primitive(_) => true,
            SerializedNode::Split(node) => node.is_primitive(),
            _ => false,
        }
    }
}

impl EGraph {
    /// Refresh only subsumption flags in a complete default snapshot. Every
    /// existing function row, input and canonical output must still match;
    /// structural changes return false without modifying the snapshot. Node IDs
    /// remain those of the snapshot even if table offsets have moved. New
    /// functions may be omitted only when explicitly named by the caller
    /// (e.g. capability annotations that extraction does not consume).
    pub fn refresh_serialized_subsumption(
        &self,
        snapshot: &mut egraph_serialize::EGraph,
        omit_new_functions: &[&str],
    ) -> bool {
        self.refresh_serialized_subsumption_filtered(snapshot, omit_new_functions, |_| true)
    }

    /// The same structural validation for an explicitly projected snapshot.
    /// `include` must be the filter used to create that snapshot. Globals are
    /// always checked because they supply the retained classes' metadata.
    pub fn refresh_serialized_subsumption_filtered(
        &self,
        snapshot: &mut egraph_serialize::EGraph,
        omit_new_functions: &[&str],
        include: impl Fn(&str) -> bool,
    ) -> bool {
        let mut calls: HashMap<&str, Vec<&egraph_serialize::NodeId>> = HashMap::default();
        for (id, node) in &snapshot.nodes {
            if id.as_ref().starts_with("function-") {
                calls.entry(node.op.as_str()).or_default().push(id);
            }
        }
        let mut globals = HashMap::default();
        for (class, data) in &snapshot.class_data {
            if let Some(names) = data.extra.get("let") {
                for name in names.split(", ") {
                    globals.insert(name, class);
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
                let mut matches = true;
                let mut count = 0;
                self.backend.for_each_while(function.backend_id, |row| {
                    count += 1;
                    let class = self
                        .value_to_class_id(&function.func_type.output, *row.vals.last().unwrap());
                    matches = match previous {
                        Some(previous) => *previous == class,
                        // Unreferenced primitive globals are absent from a
                        // default snapshot. They do not add class metadata.
                        None => !snapshot.class_data.contains_key(&class),
                    };
                    matches
                });
                if !matches || count > 1 || (count == 0 && previous.is_some()) {
                    return false;
                }
                continue;
            }
            let previous = calls.remove(name.as_str()).unwrap_or_default();
            if previous.is_empty() && omit_new_functions.contains(&name.as_str()) {
                continue;
            }
            let mut rows_by_value = HashMap::default();
            for id in previous {
                let node = &snapshot.nodes[id];
                if node.children.len() != function.func_type.input.len() {
                    return false;
                }
                let mut key = Vec::with_capacity(node.children.len() + 1);
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
                    let Some((tag, bits)) = class.as_ref().rsplit_once('-') else {
                        return false;
                    };
                    if tag != sort.name() {
                        return false;
                    }
                    let Ok(bits) = bits.parse() else { return false };
                    key.push(Value::new_const(bits));
                }
                if rows_by_value.insert(key, id).is_some() {
                    return false;
                }
            }
            let mut matches = true;
            self.backend.for_each_while(function.backend_id, |row| {
                let key: Vec<_> = row
                    .vals
                    .iter()
                    .zip(
                        function
                            .func_type
                            .input
                            .iter()
                            .chain(std::iter::once(&function.func_type.output)),
                    )
                    .map(|(&value, sort)| {
                        self.backend
                            .get_canon_repr(value, sort.column_ty(&self.backend))
                    })
                    .collect();
                let Some(id) = rows_by_value.remove(&key) else {
                    matches = false;
                    return false;
                };
                if snapshot.nodes[id].subsumed != row.subsumed {
                    updates.push((id.clone(), row.subsumed));
                }
                true
            });
            if !matches || !rows_by_value.is_empty() {
                return false;
            }
        }
        if !calls.is_empty() || !globals.is_empty() {
            return false;
        }
        drop(calls);
        drop(globals);
        for (id, subsumed) in updates {
            snapshot.nodes.get_mut(&id).unwrap().subsumed = subsumed;
        }
        true
    }

    /// Serialize the egraph into a format that can be read by the egraph-serialize crate.
    ///
    /// There are multiple different semantically valid ways to do this. This is how this implementation does it:
    ///
    /// For node costs:
    /// - Primitives: 1.0
    /// - Function without costs: 1.0
    /// - Function with costs: the cost
    /// - Omitted nodes: infinite
    ///
    /// For node IDs:
    /// - Functions: Function name + hash of input values
    /// - Args which are eq sorts: Choose one ID from the e-class, distribute roughly evenly.
    /// - Args and outputs values which are primitives: Sort name + hash of value
    ///
    /// For e-classes IDs:
    /// - tag and value of canonicalized value
    ///
    /// This is to achieve the following properties:
    /// - Equivalent primitive values will show up once in the e-graph.
    /// - Functions which return primitive values will be added to the e-class of that value.
    /// - Nodes will have consistant IDs throughout execution of e-graph (used for animating changes in the visualization)
    /// - Edges in the visualization will be well distributed (used for animating changes in the visualization)
    ///   (Note that this will be changed in `<https://github.com/egraphs-good/egglog/pull/158>` so that edges point to exact nodes instead of looking up the e-class)
    pub fn serialize(&self, config: SerializeConfig) -> SerializeOutput {
        self.serialize_filtered(config, |_| true)
    }

    /// Serialize a caller-selected projection. Excluded functions are reported
    /// as discarded; their rows are never copied or converted. Globals still
    /// annotate retained classes. As with size-limited serialization, callers
    /// must check for omitted (`[...]`) children before using a projection as
    /// an executable representation.
    pub fn serialize_filtered(
        &self,
        config: SerializeConfig,
        include: impl Fn(&str) -> bool,
    ) -> SerializeOutput {
        let mut truncated_functions = Vec::new();
        let mut discarded_functions = Vec::new();
        let max_calls_per_function = config.max_calls_per_function.unwrap_or(usize::MAX);
        let max_functions = config.max_functions.unwrap_or(usize::MAX);
        let mut all_calls: Vec<(
            &Function,
            Vec<Value>, // inputs
            Value,      // output
            bool,       // is subsumed
            egraph_serialize::ClassId,
            egraph_serialize::NodeId,
        )> = Vec::new();
        let mut functions_kept = 0usize;
        let mut let_bindings = HashMap::default();
        let mut class_ids = ClassIDs::default();
        for (name, function) in self.functions.iter() {
            if !function.decl.internal_let && !include(name) {
                discarded_functions.push(name.clone());
                continue;
            }
            if functions_kept >= max_functions {
                discarded_functions.push(name.clone());
                continue;
            }
            let mut rows = 0;
            self.backend.for_each_while(function.backend_id, |row| {
                if rows >= max_calls_per_function {
                    truncated_functions.push(name.clone());
                    return false;
                }
                let (out, inps) = row.vals.split_last().unwrap();
                let class_id = class_ids.get(self, &function.func_type.output, *out);
                if function.decl.internal_let {
                    let_bindings
                        .entry(class_id.clone())
                        .or_insert_with(Vec::new)
                        .push(name.clone());
                } else {
                    all_calls.push((
                        function,
                        inps.to_vec(),
                        *out,
                        row.subsumed,
                        class_id,
                        self.to_node_id(
                            None,
                            SerializedNode::Function {
                                name: name.clone(),
                                offset: rows,
                            },
                        ),
                    ));
                    rows += 1;
                }
                true
            });
            if rows != 0 {
                functions_kept += 1;
            }
        }

        // Then create a mapping from each canonical e-class ID to the set of node IDs in that e-class
        // Note that this is only for e-classes, primitives have e-classes equal to their node ID
        // This is for when we need to find what node ID to use for an edge to an e-class, we can rotate them evenly
        // amoung all possible options.
        let node_ids: NodeIDs = all_calls.iter().fold(
            HashMap::default(),
            |mut acc, (func, _input, _output, _subsumed, class_id, node_id)| {
                if func.func_type.output.is_eq_sort() {
                    acc.entry(class_id.clone())
                        .or_default()
                        .ids
                        .push(node_id.clone());
                }
                acc
            },
        );

        let mut serializer = Serializer {
            node_ids,
            result: egraph_serialize::EGraph::default(),
            let_bindings,
            class_ids,
            primitive_ids: HashMap::default(),
        };
        serializer.result.nodes.reserve(all_calls.len());
        serializer
            .result
            .class_data
            .reserve(serializer.node_ids.len());

        for (func, input, output, subsumed, class_id, node_id) in all_calls {
            self.serialize_value(&mut serializer, &func.func_type.output, output, &class_id);

            assert_eq!(input.len(), func.func_type.input.len());
            let children: Vec<_> = input
                .iter()
                .zip(&func.func_type.input)
                .map(|(&v, sort)| {
                    let class_id = serializer.class_ids.get(self, sort, v);
                    self.serialize_value(&mut serializer, sort, v, &class_id)
                })
                .collect();
            serializer.result.nodes.insert(
                node_id,
                egraph_serialize::Node {
                    op: func.decl.name.to_string(),
                    eclass: class_id.clone(),
                    cost: NotNan::new(func.decl.cost.unwrap_or(1) as f64).unwrap(),
                    children,
                    subsumed,
                },
            );
        }

        serializer.result.root_eclasses = config
            .root_eclasses
            .iter()
            .map(|(sort, v)| self.value_to_class_id(sort, *v))
            .collect();
        SerializeOutput {
            egraph: serializer.result,
            truncated_functions,
            discarded_functions,
        }
    }

    /// Gets the serialized class ID for a value.
    pub fn value_to_class_id(&self, sort: &ArcSort, value: Value) -> egraph_serialize::ClassId {
        // Canonicalize the value first so that we always use the canonical e-class ID
        let value = self
            .backend
            .get_canon_repr(value, sort.column_ty(&self.backend));
        use numeric_id::NumericId;
        // `value.rep()` is numeric and never contains '-', so the sort tag and the
        // value are recovered by splitting on the *last* '-' (see `class_id_to_value`).
        // This keeps class IDs unambiguous even when the sort name contains '-'.
        format!("{}-{}", sort.name(), value.rep()).into()
    }

    /// Gets the value for a serialized class ID.
    pub fn class_id_to_value(&self, eclass_id: &egraph_serialize::ClassId) -> Value {
        let s = eclass_id.to_string();
        let (_tag, bits) = s.rsplit_once('-').unwrap();
        Value::new_const(bits.parse().unwrap())
    }

    /// Gets the serialized node ID for the primitive, omitted, or function value.
    pub fn to_node_id(
        &self,
        sort: Option<&ArcSort>,
        node: SerializedNode,
    ) -> egraph_serialize::NodeId {
        match node {
            SerializedNode::Function { name, offset } => {
                assert!(sort.is_none());
                format!("function-{offset}-{name}").into()
            }
            SerializedNode::Primitive(value) => {
                format!("primitive-{}", self.value_to_class_id(sort.unwrap(), value)).into()
            }
            SerializedNode::Dummy(value) => {
                format!("dummy-{}", self.value_to_class_id(sort.unwrap(), value)).into()
            }
            SerializedNode::Split(node) => format!("split-{}", self.to_node_id(sort, *node)).into(),
        }
    }

    /// Gets the serialized node for the node ID.
    pub fn from_node_id(&self, node_id: &egraph_serialize::NodeId) -> SerializedNode {
        let node_id = node_id.to_string();
        let (tag, rest) = node_id.split_once('-').unwrap();
        match tag {
            "function" => {
                let (offset, name) = rest.split_once('-').unwrap();
                SerializedNode::Function {
                    name: name.into(),
                    offset: offset.parse().unwrap(),
                }
            }
            "primitive" => {
                let class_id: egraph_serialize::ClassId = rest.into();
                SerializedNode::Primitive(self.class_id_to_value(&class_id))
            }
            "dummy" => {
                let class_id: egraph_serialize::ClassId = rest.into();
                SerializedNode::Dummy(self.class_id_to_value(&class_id))
            }
            "split" => {
                let (_offset, rest) = rest.split_once('-').unwrap();
                let node_id: egraph_serialize::NodeId = rest.into();
                SerializedNode::Split(Box::new(self.from_node_id(&node_id)))
            }
            _ => std::panic::panic_any(format!("Unknown node ID: {tag}-{rest}")),
        }
    }

    /// Serialize the value and return the node ID
    /// If this is a primitive value, we will add the node to the data, but if it is an eclass, we will not
    /// When this is called on the output of a node, we only use the e-class to know which e-class its a part of
    /// When this is called on an input of a node, we only use the node ID to know which node to point to.
    fn serialize_value(
        &self,
        serializer: &mut Serializer,
        sort: &ArcSort,
        value: Value,
        class_id: &egraph_serialize::ClassId,
    ) -> egraph_serialize::NodeId {
        let node_id = if sort.is_eq_sort() {
            let node_ids = serializer
                .node_ids
                .entry(class_id.clone())
                .or_insert_with(|| {
                    // If we don't find node IDs for this class, it means that all nodes for it were omitted due to size constraints
                    // In this case, add a dummy node in this class to represent the missing nodes
                    let node_id = self.to_node_id(Some(sort), SerializedNode::Dummy(value));
                    serializer.result.nodes.insert(
                        node_id.clone(),
                        egraph_serialize::Node {
                            op: "[...]".to_string(),
                            eclass: class_id.clone(),
                            cost: NotNan::new(f64::INFINITY).unwrap(),
                            children: vec![],
                            subsumed: false,
                        },
                    );
                    ClassNodes {
                        ids: smallvec::smallvec![node_id],
                        cursor: 0,
                    }
                });
            node_ids.next()
        } else {
            // Containers still traverse their children on every occurrence:
            // this preserves the serializer's rotation of eq-sort edges.
            if !sort.is_container_sort()
                && let Some(id) = serializer.primitive_ids.get(class_id)
            {
                return id.clone();
            }
            let node_id: egraph_serialize::NodeId = format!("primitive-{class_id}").into();
            // Add node for value
            {
                let container_values = self.backend.container_values();
                // Children will be empty unless this is a container sort
                let children: Vec<egraph_serialize::NodeId> = sort
                    .inner_values(container_values, value)
                    .into_iter()
                    .map(|(s, v)| {
                        let child_class = serializer.class_ids.get(self, &s, v);
                        self.serialize_value(serializer, &s, v, &child_class)
                    })
                    .collect();
                // If this is a container sort, use the name, otherwise use the value
                let op = if sort.is_container_sort() {
                    sort.serialized_name(container_values, value)
                } else {
                    let primitive_id = self
                        .backend
                        .base_values()
                        .get_ty_by_id(sort.value_type().unwrap());
                    let formatted_val = BaseValuePrinter {
                        base: self.backend.base_values(),
                        ty: primitive_id,
                        val: value,
                    };
                    format!("{formatted_val:?}")
                };
                serializer.result.nodes.insert(
                    node_id.clone(),
                    egraph_serialize::Node {
                        op,
                        eclass: class_id.clone(),
                        cost: NotNan::new(1.0).unwrap(),
                        children,
                        subsumed: false,
                    },
                );
            };
            if !sort.is_container_sort() {
                serializer
                    .primitive_ids
                    .insert(class_id.clone(), node_id.clone());
            }
            node_id
        };
        if serializer.result.class_data.contains_key(class_id) {
            return node_id;
        }
        #[allow(clippy::disallowed_types)]
        let mut extra = std::collections::HashMap::default();
        if let Some(let_bindings) = serializer.let_bindings.get(class_id)
            && !let_bindings.is_empty()
        {
            extra.insert("let".to_string(), let_bindings.join(", "));
        }
        serializer.result.class_data.insert(
            class_id.clone(),
            egraph_serialize::ClassData {
                typ: Some(sort.name().to_string()),
                extra,
            },
        );
        node_id
    }
}

/// Most classes have one node. Keep that representative inline while retaining
/// exactly the original rotate-before-read sequence for multi-node classes.
#[derive(Default)]
struct ClassNodes {
    ids: SmallVec<[egraph_serialize::NodeId; 1]>,
    cursor: usize,
}

impl ClassNodes {
    fn next(&mut self) -> egraph_serialize::NodeId {
        self.cursor += 1;
        if self.cursor == self.ids.len() {
            self.cursor = 0;
        }
        self.ids[self.cursor].clone()
    }
}

type NodeIDs = HashMap<egraph_serialize::ClassId, ClassNodes>;

#[cfg(test)]
mod class_node_tests {
    use super::*;

    #[test]
    fn representative_sequence_matches_serialized_edge_rotation() {
        for len in [1, 2, 3, 17, 64] {
            let ids: Vec<egraph_serialize::NodeId> =
                (0..len).map(|i| format!("node-{i}").into()).collect();
            let mut reference: std::collections::VecDeque<_> = ids.clone().into();
            let mut nodes = ClassNodes {
                ids: ids.into_iter().collect(),
                cursor: 0,
            };
            for _ in 0..len * 5 {
                reference.rotate_left(1);
                assert_eq!(nodes.next(), *reference.front().unwrap());
            }
        }
    }
}
