// Frozen export implementation at 9da0df11. Test-only oracle for node order,
// primitive/container overwrites, class metadata and representative rotation.
struct ReferenceSerializer {
    node_ids: NodeIDs,
    result: egraph_serialize::EGraph,
    let_bindings: HashMap<egraph_serialize::ClassId, Vec<String>>,
}
impl EGraph {
    pub(super) fn serialize_reference(&self, config: SerializeConfig) -> SerializeOutput {
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
                let class_id = self.value_to_class_id(&function.func_type.output, *out);
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
                        .push_back(node_id.clone());
                }
                acc
            },
        );

        let mut serializer = ReferenceSerializer {
            node_ids,
            result: egraph_serialize::EGraph::default(),
            let_bindings,
        };

        for (func, input, output, subsumed, class_id, node_id) in all_calls {
            self.serialize_value_reference(
                &mut serializer,
                &func.func_type.output,
                output,
                &class_id,
            );

            assert_eq!(input.len(), func.func_type.input.len());
            let children: Vec<_> = input
                .iter()
                .zip(&func.func_type.input)
                .map(|(&v, sort)| {
                    self.serialize_value_reference(
                        &mut serializer,
                        sort,
                        v,
                        &self.value_to_class_id(sort, v),
                    )
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

    fn serialize_value_reference(
        &self,
        serializer: &mut ReferenceSerializer,
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
                    VecDeque::from(vec![node_id])
                });
            node_ids.rotate_left(1);
            node_ids.front().unwrap().clone()
        } else {
            let node_id = self.to_node_id(Some(sort), SerializedNode::Primitive(value));
            // Add node for value
            {
                let container_values = self.backend.container_values();
                // Children will be empty unless this is a container sort
                let children: Vec<egraph_serialize::NodeId> = sort
                    .inner_values(container_values, value)
                    .into_iter()
                    .map(|(s, v)| {
                        self.serialize_value_reference(
                            serializer,
                            &s,
                            v,
                            &self.value_to_class_id(&s, v),
                        )
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
            node_id
        };
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
