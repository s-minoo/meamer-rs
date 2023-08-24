use std::cell::RefCell;
use std::fmt::{Debug, Display};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::Result;
use operator::display::PrettyDisplay;
use operator::{Operator, Serializer, Source, Target};
use petgraph::dot::{Config, Dot};
use petgraph::graph::{DiGraph, NodeIndex};
use serde_json::json;

use crate::error::PlanError;

type DiGraphOperators = DiGraph<PlanNode, PlanEdge>;
pub type RcRefCellDiGraph = Rc<RefCell<DiGraphOperators>>;

type VSourceIdxs = Vec<NodeIndex>;
pub type RcRefCellVSourceIdxs = Rc<RefCell<VSourceIdxs>>;

// Plan states in unit structs

#[derive(Debug, Clone)]
pub struct Init {}
#[derive(Debug, Clone)]
pub struct Processed {}
#[derive(Debug, Clone)]
pub struct Serialized {}
#[derive(Debug, Clone)]
pub struct Sunk {}

#[derive(Debug, Clone)]
pub struct Plan<T> {
    _t:            PhantomData<T>,
    pub graph:     RcRefCellDiGraph,
    pub sources:   RcRefCellVSourceIdxs,
    pub last_node: Option<NodeIndex>,
}

impl<T> Plan<T> {
    fn empty_plan_apply_check(&self) -> Result<(), PlanError> {
        if self.graph.borrow().node_count() == 0 {
            return Err(PlanError::EmptyPlan);
        }
        Ok(())
    }

    pub fn write_fmt(
        &mut self,
        path: PathBuf,
        fmt: &dyn Fn(Dot<&DiGraphOperators>) -> String,
    ) -> Result<()> {
        let graph = &*self.graph.borrow_mut();
        let dot_string = fmt(Dot::with_config(graph, &[Config::EdgeNoLabel]));
        write_string_to_file(path, dot_string)?;
        Ok(())
    }

    pub fn write_pretty(&mut self, path: PathBuf) -> Result<()> {
        self.write_fmt(path, &|dot| format!("{}", dot))?;
        Ok(())
    }

    pub fn write(&mut self, path: PathBuf) -> Result<()> {
        self.write_fmt(path, &|dot| format!("{:?}", dot))?;
        Ok(())
    }
}

impl Plan<()> {
    pub fn new() -> Plan<Init> {
        Plan {
            _t:        PhantomData,
            graph:     Rc::new(RefCell::new(DiGraph::new())),
            sources:   Rc::new(RefCell::new(Vec::new())),
            last_node: None,
        }
    }
}

fn write_string_to_file(
    path: PathBuf,
    content: String,
) -> Result<(), anyhow::Error> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    write!(writer, "{}", content)?;
    Ok(())
}

impl Plan<Init> {
    pub fn source(&mut self, source: Source) -> Plan<Processed> {
        let graph = &mut *self.graph.borrow_mut();
        let source_op = Operator::SourceOp {
            config: source.clone(),
        };
        let sources = &mut *self.sources.borrow_mut();

        let plan_node = PlanNode {
            id:       format!("Source_{}", graph.node_count()),
            operator: source_op,
        };
        let idx = Some(graph.add_node(plan_node));
        sources.push(idx.unwrap());

        Plan {
            _t:        PhantomData,
            graph:     Rc::clone(&self.graph),
            sources:   Rc::clone(&self.sources),
            last_node: idx,
        }
    }
}

impl Plan<Processed> {
    pub fn apply(
        &mut self,
        operator: &Operator,
        node_id_prefix: &str,
    ) -> Result<Plan<Processed>, PlanError> {
        self.empty_plan_apply_check()?;
        let prev_node_idx = self
            .last_node
            .ok_or(PlanError::DanglingApplyOperator(operator.clone()))?;

        match operator {
            Operator::SourceOp { .. }
            | Operator::TargetOp { .. }
            | Operator::SerializerOp { .. } => {
                return Err(PlanError::WrongApplyOperator(operator.clone()))
            }
            _ => (),
        };

        let graph = &mut *self.graph.borrow_mut();
        let id_num = graph.node_count();

        let plan_node = PlanNode {
            id:       format!("{}_{}", node_id_prefix, id_num),
            operator: operator.clone(),
        };

        let new_node_idx = graph.add_node(plan_node);

        let plan_edge = PlanEdge {
            key:   std::any::type_name::<()>().to_string(),
            value: "MappingTuple".to_string(),
        };

        graph.add_edge(prev_node_idx, new_node_idx, plan_edge);

        Ok(Plan {
            _t:        PhantomData,
            graph:     Rc::clone(&self.graph),
            sources:   Rc::clone(&self.sources),
            last_node: Some(new_node_idx),
        })
    }

    pub fn serialize(
        &mut self,
        serializer: Serializer,
    ) -> Result<Plan<Serialized>, PlanError> {
        self.empty_plan_apply_check()?;
        let prev_node_idx = self.last_node.ok_or(
            PlanError::DanglingApplyOperator(Operator::SerializerOp {
                config: serializer.clone(),
            }),
        )?;

        let graph = &mut *self.graph.borrow_mut();
        let plan_node = PlanNode {
            id:       format!("Serialize_{}", graph.node_count()),
            operator: Operator::SerializerOp { config: serializer },
        };

        let node_idx = graph.add_node(plan_node);

        let plan_edge = PlanEdge {
            key:   std::any::type_name::<()>().to_string(),
            value: "MappingTuple".to_string(),
        };

        graph.add_edge(prev_node_idx, node_idx, plan_edge);
        Ok(Plan {
            _t:        PhantomData,
            graph:     Rc::clone(&self.graph),
            sources:   Rc::clone(&self.sources),
            last_node: Some(node_idx),
        })
    }
}

impl Plan<Serialized> {
    pub fn sink(&mut self, sink: Target) -> Result<Plan<Sunk>, PlanError> {
        if self.last_node.is_none() {
            return Err(PlanError::EmptyPlan);
        }

        let graph = &mut *self.graph.borrow_mut();
        let plan_node = PlanNode {
            id:       format!("Sink_{}", graph.node_count()),
            operator: Operator::TargetOp { config: sink },
        };

        let node_idx = graph.add_node(plan_node);
        let prev_node_idx = self.last_node.unwrap();

        let plan_edge = PlanEdge {
            key:   std::any::type_name::<()>().to_string(),
            value: "Serialized Format".to_string(),
        };
        graph.add_edge(prev_node_idx, node_idx, plan_edge);

        Ok(Plan {
            _t:        PhantomData,
            graph:     Rc::clone(&self.graph),
            sources:   Rc::clone(&self.sources),
            last_node: Some(node_idx),
        })
    }
}

#[derive(Debug, Clone)]
pub struct PlanEdge {
    pub key:   String,
    pub value: String,
}

impl Display for PlanEdge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} {}", self.key, "->", self.value)
    }
}

#[derive(Clone, Hash)]
pub struct PlanNode {
    pub id:       String,
    pub operator: Operator,
}

impl Debug for PlanNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let json = json!({"id": self.id, "operator": self.operator});
        f.write_str(&serde_json::to_string(&json).unwrap())
    }
}

impl PrettyDisplay for PlanNode {
    fn pretty_string(&self) -> Result<String> {
        let content = self.operator.pretty_string()?;

        Ok(format!("Id: {}\n{}", self.id, content))
    }
}

impl Display for PlanNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "id:{} \n{}",
            self.id,
            self.operator.pretty_string().unwrap()
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use operator::{Projection, Rename};

    use super::*;

    #[test]
    fn test_plan_source() {
        let mut plan = Plan::new();
        let source = Source {
            config:              HashMap::new(),
            source_type:         operator::IOType::File,
            reference_iterators: vec![],
            data_format:         operator::formats::DataFormat::CSV,
        };
        plan.source(source.clone());
        let graph = plan.graph.borrow();

        assert!(graph.node_count() == 1);
        assert!(graph.edge_count() == 0);
        let retrieved_node = graph.node_weights().next();

        assert!(retrieved_node.is_some());
        let source_op = Operator::SourceOp { config: source };
        assert!(retrieved_node.unwrap().operator == source_op);
    }

    #[test]
    fn test_plan_apply() -> std::result::Result<(), PlanError> {
        let mut plan = Plan::new();
        let source = Source {
            config:              HashMap::new(),
            source_type:         operator::IOType::File,
            reference_iterators: vec![],
            data_format:         operator::formats::DataFormat::CSV,
        };

        let project_op = Operator::ProjectOp {
            config: Projection {
                projection_attributes: HashSet::new(),
            },
        };
        let rename_op = Operator::RenameOp {
            config: Rename {
                rename_pairs: HashMap::from([(
                    "first".to_string(),
                    "last".to_string(),
                )]),
            },
        };

        let _ = plan
            .source(source.clone())
            .apply(&project_op, "Projection")?
            .apply(&rename_op, "Rename")?;

        let graph = plan.graph.borrow();

        assert!(
            graph.node_count() == 3,
            "Number of nodes should be 3 but it is instead: {}",
            graph.node_count()
        );
        assert!(
            graph.edge_count() == 2,
            "Number of edges should be 2 but it is instead: {}",
            graph.edge_count()
        );

        Ok(())
    }
}