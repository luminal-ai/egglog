use crate::{util::HashMap, *};
use core_relations::BaseValuePrinter;
use ordered_float::NotNan;
use std::collections::VecDeque;

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
    result: egraph_serialize::EGraph,
    let_bindings: HashMap<egraph_serialize::ClassId, Vec<String>>,
    class_ids: ClassIDs,
}

struct SerializedValue {
    class_id: egraph_serialize::ClassId,
    node_ids: VecDeque<egraph_serialize::NodeId>,
    primitive_node_id: Option<egraph_serialize::NodeId>,
    metadata_written: bool,
}

/// The export observes an immutable e-graph. Index its values by sort and
/// canonical numeric ID; format class IDs and build metadata only once.
/// Keep this state local to an export so later exports see intervening unions.
#[derive(Default)]
struct ClassIDs(HashMap<String, HashMap<Value, SerializedValue>>);

impl ClassIDs {
    fn get_mut(&mut self, egraph: &EGraph, sort: &ArcSort, value: Value) -> &mut SerializedValue {
        let value = egraph
            .backend
            .get_canon_repr(value, sort.column_ty(&egraph.backend));
        if !self.0.contains_key(sort.name()) {
            self.0.insert(sort.name().to_owned(), HashMap::default());
        }
        self.0
            .get_mut(sort.name())
            .unwrap()
            .entry(value)
            .or_insert_with(|| SerializedValue {
                class_id: egraph.value_to_class_id(sort, value),
                node_ids: VecDeque::new(),
                primitive_node_id: None,
                metadata_written: false,
            })
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
                let class_id = class_ids
                    .get_mut(self, &function.func_type.output, *out)
                    .class_id
                    .clone();
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
        for (func, _input, output, _subsumed, _class_id, node_id) in &all_calls {
            if func.func_type.output.is_eq_sort() {
                class_ids
                    .get_mut(self, &func.func_type.output, *output)
                    .node_ids
                    .push_back(node_id.clone());
            }
        }

        let mut serializer = Serializer {
            result: egraph_serialize::EGraph::default(),
            let_bindings,
            class_ids,
        };

        for (func, input, output, subsumed, class_id, node_id) in all_calls {
            self.serialize_value(&mut serializer, &func.func_type.output, output);

            assert_eq!(input.len(), func.func_type.input.len());
            let children: Vec<_> = input
                .iter()
                .zip(&func.func_type.input)
                .map(|(&v, sort)| self.serialize_value(&mut serializer, sort, v))
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
    ) -> egraph_serialize::NodeId {
        // Metadata and non-container primitive nodes are immutable during a
        // serialization. Construct them once rather than replacing them for
        // every edge. Containers still visit their children on every occurrence:
        // those visits rotate the representatives of equality classes.
        let record = serializer.class_ids.get_mut(self, sort, value);
        let class_id = record.class_id.clone();
        let first_visit = !record.metadata_written;
        record.metadata_written = true;
        let node_id = if sort.is_eq_sort() {
            if record.node_ids.is_empty() {
                // This class has no retained function rows (e.g. a truncated
                // export). Retain the original dummy-node behavior.
                let node_id: egraph_serialize::NodeId = format!("dummy-{class_id}").into();
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
                record.node_ids.push_back(node_id);
            }
            // Preserve every rotation, including visits through repeated
            // containers. Their final child representatives can change.
            record.node_ids.rotate_left(1);
            record.node_ids.front().unwrap().clone()
        } else {
            let node_id = record
                .primitive_node_id
                .get_or_insert_with(|| format!("primitive-{class_id}").into())
                .clone();
            // Add node for value
            if first_visit || sort.is_container_sort() {
                let container_values = self.backend.container_values();
                // Children will be empty unless this is a container sort
                let children: Vec<egraph_serialize::NodeId> = sort
                    .inner_values(container_values, value)
                    .into_iter()
                    .map(|(s, v)| self.serialize_value(serializer, &s, v))
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
            node_id
        };
        if !first_visit {
            return node_id;
        }
        #[allow(clippy::disallowed_types)]
        let mut extra = std::collections::HashMap::default();
        if let Some(let_bindings) = serializer.let_bindings.get(&class_id)
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

#[cfg(test)]
type NodeIDs = HashMap<egraph_serialize::ClassId, VecDeque<egraph_serialize::NodeId>>;

#[cfg(test)]
mod tests {
    use super::*;
    mod reference {
        use super::super::*;
        include!("serialize_reference.rs");
    }

    fn assert_reference_export(egraph: &EGraph) {
        let mut roots = Vec::new();
        let num = &egraph.functions["Num"];
        egraph.backend.for_each_while(num.backend_id, |row| {
            roots.push((num.func_type.output.clone(), *row.vals.last().unwrap()));
            true
        });
        for max_functions in [None, Some(0), Some(1), Some(4)] {
            for max_calls_per_function in [None, Some(0), Some(1), Some(2)] {
                for include_temporary_functions in [false, true] {
                    let config = || SerializeConfig {
                        max_functions,
                        max_calls_per_function,
                        include_temporary_functions,
                        root_eclasses: roots.clone(),
                    };
                    let expected = egraph.serialize_reference(config());
                    let actual = egraph.serialize(config());
                    // Equality alone does not check IndexMap insertion order.
                    assert_eq!(
                        serde_json::to_vec(&actual.egraph).unwrap(),
                        serde_json::to_vec(&expected.egraph).unwrap()
                    );
                    assert_eq!(actual.truncated_functions, expected.truncated_functions);
                    assert_eq!(actual.discarded_functions, expected.discarded_functions);
                }
            }
        }
    }

    #[test]
    fn export_matches_reference_with_aliases_containers_and_unions() {
        let program = r#"
            (datatype Math (Num i64) (Add Math Math))
            (sort MathVec (Vec Math))
            (sort MathPair (Pair Math i64))
            (constructor Wrap (MathVec MathPair String f64) Math)
            (let $a (Num 1))
            (let $alias (Num 1))
            (let $b (Num 2))
            (let $w (Wrap (vec-of $a $b $a) (pair $a 1) "shared" 1.5))
            (let $again (Wrap (vec-of $a $b $a) (pair $a 1) "shared" 1.5))
            (Add $w $again)
            (Add $a $b)
            (Add $b $a)
            (subsume (Add $b $a))
        "#;
        for mut egraph in [
            EGraph::default(),
            EGraph::new_with_term_encoding(),
            EGraph::new_with_proofs(),
        ] {
            egraph.parse_and_run_program(None, program).unwrap();
            assert_reference_export(&egraph);
            egraph
                .parse_and_run_program(None, "(union $a $b) (run 1)")
                .unwrap();
            assert_reference_export(&egraph);
            // The canonical-ID cache must never survive an export.
            assert_reference_export(&egraph);
        }
    }
}
