use std::collections::{HashMap, HashSet};

use operator::{
    Extend, Function, Operator, Projection, RcExtendFunction, Serializer,
    Source, Target,
};
use plangenerator::error::PlanError;
use plangenerator::plan::{Init, Plan, Processed};
use sophia_api::term::TTerm;
use sophia_term::iri::Iri;
use sophia_term::Term;

use crate::rml_model::join::JoinCondition;
use crate::rml_model::term_map::{
    self, ObjectMap, SubjectMap, TermMapInfo, TermMapType,
};
use crate::rml_model::{Document, PredicateObjectMap, TriplesMap};

fn file_target(count: usize) -> Target {
    let mut config = HashMap::new();
    config.insert("path".to_string(), format!("{}_output.nt", count));
    Target {
        configuration: config,
        target_type:   operator::IOType::File,
        data_format:   operator::formats::DataFormat::NT,
    }
}

fn partition_pom_join_nonjoin(
    poms: Vec<PredicateObjectMap>,
) -> (
    Vec<(usize, PredicateObjectMap)>,
    Vec<(usize, PredicateObjectMap)>,
) {
    let (mut ptm_poms, mut no_ptm_poms): (Vec<_>, Vec<_>) = poms
        .into_iter()
        .enumerate()
        .partition(|(_, pom)| pom.contains_ptm());

    for (pom_idx, pom) in ptm_poms.iter_mut() {
        let (ptm_oms, no_ptm_oms): (Vec<_>, Vec<_>) = pom
            .object_maps
            .clone()
            .into_iter()
            .partition(|om| om.parent_tm.is_some());

        pom.object_maps = ptm_oms;
        if !no_ptm_oms.is_empty() {
            no_ptm_poms.push((
                *pom_idx,
                PredicateObjectMap {
                    predicate_maps: pom.predicate_maps.clone(),
                    object_maps:    no_ptm_oms,
                },
            ));
        }
    }

    (ptm_poms, no_ptm_poms)
}

pub fn translate_to_algebra(doc: Document) -> Result<Plan<Init>, PlanError> {
    let mut plan = Plan::<()>::new();
    let tm_projected_pairs_res: Result<Vec<_>, PlanError> = doc
        .triples_maps
        .into_iter()
        .map(|tm| {
            let source_op = translate_source_op(&tm);
            let projection_op = translate_projection_op(&tm);
            Ok((
                tm,
                plan.source(source_op).apply(&projection_op, "Projection")?,
            ))
        })
        .collect();

    let tm_projected_pairs = tm_projected_pairs_res?;
    let search_tm_plan_map: HashMap<_, _> = tm_projected_pairs
        .clone()
        .into_iter()
        .enumerate()
        .map(|(count, (tm, plan))| (tm.identifier.clone(), (count, tm, plan)))
        .collect();

    let _ = tm_projected_pairs
        .clone()
        .iter_mut()
        .enumerate()
        .try_for_each(|(count, (tm, plan))| {
            let prefix_id = &format!("tm_{}", count);
            let sm = &tm.subject_map;
            let (joined_idx_poms, no_join_idx_poms): (Vec<_>, Vec<_>) =
                partition_pom_join_nonjoin(tm.po_maps.clone());

            if !no_join_idx_poms.is_empty() {
                add_non_join_related_ops(
                    no_join_idx_poms
                        .iter()
                        .map(|(idx, pom)| (*idx, pom))
                        .collect(),
                    sm,
                    prefix_id,
                    plan,
                    count,
                )?;
            }

            if !joined_idx_poms.is_empty() {
                add_join_related_ops(
                    joined_idx_poms
                        .iter()
                        .map(|(idx, pom)| (*idx, pom))
                        .collect(),
                    &search_tm_plan_map,
                    sm,
                    prefix_id,
                    plan,
                    count,
                )?;
            }

            Ok::<(), PlanError>(())
        });

    Ok(plan)
}

fn add_join_related_ops(
    join_idx_poms: Vec<(usize, &PredicateObjectMap)>,
    search_tm_plan_map: &HashMap<String, (usize, TriplesMap, Plan<Processed>)>,
    sm: &SubjectMap,
    prefix_id: &str,
    plan: &mut Plan<Processed>,
    count: usize,
) -> Result<(), PlanError> {
    // HashMap pairing the attribute with the function generated from
    // PTM's subject map

    for (pom_idx, pom) in join_idx_poms {
        let pms = &pom.predicate_maps;
        let oms = &pom.object_maps;

        for (om_idx, om) in oms.iter().enumerate() {
            let ptm_iri = om
                .parent_tm
                .as_ref()
                .ok_or(PlanError::AuxError(format!(
                    "Parent triples map needs to be present in OM: {:#?}",
                    om
                )))?
                .to_string();

            let (idx, ptm, other_plan) =
                search_tm_plan_map.get(&ptm_iri).ok_or(PlanError::AuxError(
                    format!("Parent triples map IRI is wrong: {}", &ptm_iri),
                ))?;

            let join_cond = om.join_condition.as_ref().unwrap();
            let child_attributes = &join_cond.child_attributes;
            let parent_attributes = &join_cond.parent_attributes;
            let ptm_alias = format!("join_{}", idx);

            let mut joined_plan = plan
                .join(other_plan)?
                .alias(&ptm_alias)?
                .where_by(child_attributes.clone())?
                .compared_to(parent_attributes.clone())?;

            // Prefix the attributes in the subject map with the alias of the PTM
            let ptm_sm_info = ptm
                .subject_map
                .tm_info
                .clone()
                .prefix_attributes(&ptm_alias);

            // Pair the ptm subject iri function with an extended attribute
            let ptm_sub_function =
                extract_extend_function_from_term_map(&ptm_sm_info);
            let om_extend_attr =
                format!("{}_o{}-{}", prefix_id, pom_idx, om_idx);

            let pom_with_joined_ptm = PredicateObjectMap {
                predicate_maps: pms.clone(),
                object_maps:    [om.clone()].to_vec(),
            };

            let idx_poms = [(pom_idx, &pom_with_joined_ptm)].into_iter();
            let mut extend_pairs =
                translate_extend_pairs(prefix_id, sm, idx_poms.clone());

            extend_pairs.insert(om_extend_attr, ptm_sub_function);

            let extend_op = Operator::ExtendOp {
                config: Extend { extend_pairs },
            };

            let serializer_op = translate_serializer_op(idx_poms, prefix_id);

            let _ = joined_plan
                .apply(&extend_op, "Extend")?
                .serialize(serializer_op)?
                .sink(file_target(count));
        }
    }

    Ok(())
}

fn add_non_join_related_ops(
    no_join_idx_poms: Vec<(usize, &PredicateObjectMap)>,
    sm: &SubjectMap,
    prefix_id: &str,
    plan: &mut Plan<Processed>,
    count: usize,
) -> Result<(), PlanError> {
    let no_join_idx_poms_iter = no_join_idx_poms.into_iter();
    let extend_op =
        translate_extend_op(&sm, no_join_idx_poms_iter.clone(), &prefix_id);
    let serializer_op =
        translate_serializer_op(no_join_idx_poms_iter, &prefix_id);
    let _ = plan
        .apply(&extend_op, "ExtendOp")?
        .serialize(serializer_op)?
        .sink(file_target(count));
    Ok(())
}

fn translate_source_op(tm: &TriplesMap) -> Source {
    tm.logical_source.clone().into()
}

fn translate_projection_op(tm: &TriplesMap) -> Operator {
    let mut projection_attributes = tm.subject_map.tm_info.get_attributes();
    let gm_attributes = tm
        .graph_map
        .clone()
        .map_or(HashSet::new(), |gm| gm.tm_info.get_attributes());

    let p_attributes: HashSet<_> = tm
        .po_maps
        .iter()
        .flat_map(|pom| {
            let om_attrs = pom.object_maps.iter().flat_map(|om| {
                if let Some(join_cond) = &om.join_condition {
                    let mut child_attr = join_cond.child_attributes.clone();
                    let mut parent_attr = join_cond.parent_attributes.clone();
                    child_attr.append(&mut parent_attr);
                    child_attr.into_iter().collect()
                } else {
                    om.tm_info.get_attributes()
                }
            });
            let pm_attrs = pom
                .predicate_maps
                .iter()
                .flat_map(|pm| pm.tm_info.get_attributes());

            om_attrs.chain(pm_attrs)
        })
        .collect();

    // Subject map's attributes alread added to projection_attributes hashset
    projection_attributes.extend(p_attributes);
    projection_attributes.extend(gm_attributes);

    Operator::ProjectOp {
        config: Projection {
            projection_attributes,
        },
    }
}

fn extract_extend_function_from_term_map(tm_info: &TermMapInfo) -> Function {
    let term_value = tm_info.term_value.value().to_string();
    let value_function: RcExtendFunction = match tm_info.term_map_type {
        TermMapType::Constant => Function::Constant { value: term_value },
        TermMapType::Reference => Function::Reference { value: term_value },
        TermMapType::Template => Function::Template { value: term_value },
    }
    .into();

    let type_function = match tm_info.term_type.unwrap() {
        sophia_api::term::TermKind::Iri => {
            Function::Iri {
                inner_function: Function::UriEncode {
                    inner_function: value_function,
                }
                .into(),
            }
        }
        sophia_api::term::TermKind::Literal => {
            Function::Literal {
                inner_function: value_function,
            }
        }
        sophia_api::term::TermKind::BlankNode => {
            Function::BlankNode {
                inner_function: value_function,
            }
        }
        typ => panic!("Unrecognized term kind {:?}", typ),
    };

    type_function
}

fn translate_extend_op<'a>(
    sm: &'a SubjectMap,
    idx_poms: impl Iterator<Item = (usize, &'a PredicateObjectMap)>,
    prefix_id: &'a str,
) -> Operator {
    let extend_pairs = translate_extend_pairs(prefix_id, sm, idx_poms);

    operator::Operator::ExtendOp {
        config: Extend { extend_pairs },
    }
}

fn translate_extend_pairs<'a>(
    prefix_id: &'a str,
    sm: &'a SubjectMap,
    idx_poms: impl Iterator<Item = (usize, &'a PredicateObjectMap)>,
) -> HashMap<String, Function> {
    let sub_extend = sm_extract_extend_pair(prefix_id, sm);

    let poms_extend =
        idx_poms.flat_map(|(pom_count, pom)| {
            let predicate_extends = pom.predicate_maps.iter().enumerate().map(
                move |(p_count, pm)| {
                    (
                        format!("{}_p{}-{}", prefix_id, pom_count, p_count),
                        extract_extend_function_from_term_map(&pm.tm_info),
                    )
                },
            );

            let object_extends =
                pom.object_maps
                    .iter()
                    .enumerate()
                    .map(move |(o_count, om)| {
                        (
                            format!("{}_o{}-{}", prefix_id, pom_count, o_count),
                            extract_extend_function_from_term_map(&om.tm_info),
                        )
                    });
            predicate_extends.chain(object_extends)
        });

    let extend_ops_map: HashMap<String, Function> =
        poms_extend.chain(sub_extend).collect();
    extend_ops_map
}

fn sm_extract_extend_pair(
    prefix_id: &str,
    sm: &SubjectMap,
) -> Vec<(String, Function)> {
    let sub_extend = vec![(
        format!("{}_sm", prefix_id),
        extract_extend_function_from_term_map(&sm.tm_info),
    )];
    sub_extend
}

fn extract_serializer_template<'a>(
    pom: impl Iterator<Item = (usize, &'a PredicateObjectMap)>,
    prefix_id: &'a str,
) -> String {
    let subject = format!("{}_sm", prefix_id);
    let predicate_objects = pom.flat_map(|(idx, pom)| {
        let p_length = pom.predicate_maps.len();
        let o_length = pom.object_maps.len();

        let predicates = (0..p_length)
            .map(move |p_count| format!("{}_p{}-{}", prefix_id, idx, p_count));
        let objects = (0..o_length)
            .map(move |o_count| format!("{}_o{}-{}", prefix_id, idx, o_count));

        let pairs = predicates.flat_map(move |p_string| {
            objects
                .clone()
                .map(move |o_string| (p_string.clone(), o_string.clone()))
        });

        pairs
    });

    let triple_graph_pattern = predicate_objects
        .map(|(predicate, object)| {
            format!(" ?{} ?{} ?{}.", subject, predicate, object)
        })
        .fold(String::new(), |a, b| a + &b + "\n");

    triple_graph_pattern
}

fn translate_serializer_op<'a>(
    idx_poms: impl Iterator<Item = (usize, &'a PredicateObjectMap)>,
    prefix_id: &'a str,
) -> Serializer {
    let template = extract_serializer_template(idx_poms, prefix_id);
    Serializer {
        template,
        options: None,
        format: operator::formats::DataFormat::NT,
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Borrow;
    use std::collections::HashSet;

    use sophia_term::Term;

    use super::*;
    use crate::extractors::io::parse_file;
    use crate::extractors::triplesmap_extractor::{self, extract_triples_maps};
    use crate::import_test_mods;
    import_test_mods!();

    #[test]
    fn test_get_attributes_term_map_info() {
        let identifier = "tm_1".to_string();
        let template_term_map_info = TermMapInfo {
            identifier,
            logical_targets: HashSet::new(),
            term_map_type: term_map::TermMapType::Template,
            term_value: new_term_value("{id}{firstname}{lastname}".to_string()),
            term_type: None,
        };

        let attributes = template_term_map_info.get_attributes();
        let check = new_hash_set(["id", "firstname", "lastname"].into());

        assert_eq!(attributes, check);

        let reference_term_map_info = TermMapInfo {
            term_map_type: term_map::TermMapType::Reference,
            term_value: new_term_value("aReferenceValue".to_string()),
            ..template_term_map_info
        };

        let attributes = reference_term_map_info.get_attributes();
        let check = new_hash_set(["aReferenceValue"].into());
        assert_eq!(attributes, check);
    }

    #[test]
    fn test_projection_operator() -> ExtractorResult<()> {
        let graph = load_graph!("sample_mapping.ttl").unwrap();
        let mut triples_map_vec = extract_triples_maps(&graph)?;
        assert_eq!(triples_map_vec.len(), 1);

        let triples_map = triples_map_vec.pop().unwrap();
        let source_op = translate_source_op(&triples_map);
        let projection_ops = translate_projection_op(&triples_map);

        let projection = match projection_ops.borrow() {
            Operator::ProjectOp { config: proj } => proj,
            _ => panic!("Parsed wrong! Operator should be projection"),
        };

        let check_attributes =
            new_hash_set(["stop", "id", "latitude", "longitude"].to_vec());

        assert_eq!(projection.projection_attributes, check_attributes);

        Ok(())
    }

    fn new_term_value(value: String) -> Term<String> {
        Term::new_literal_dt_unchecked(value, Term::new_iri("string").unwrap())
    }

    fn new_hash_set(v: Vec<&str>) -> HashSet<String> {
        v.into_iter().map(|st| st.to_string()).collect()
    }

    #[test]
    fn test_extend_operator() -> ExtractorResult<()> {
        let graph = load_graph!("sample_mapping.ttl").unwrap();
        let mut triples_map_vec = extract_triples_maps(&graph)?;
        assert_eq!(triples_map_vec.len(), 1);
        let triples_map = triples_map_vec.pop().unwrap();
        let source_op = translate_source_op(&triples_map);
        let projection_ops = translate_projection_op(&triples_map);

        let extend_op = translate_extend_op(
            &triples_map.subject_map,
            triples_map.po_maps.iter().enumerate(),
            "?tm1",
        );

        println!("{:#?}", extend_op);
        Ok(())
    }

    #[test]
    fn test_operator_translation() -> ExtractorResult<()> {
        let document = parse_file(test_case!("sample_mapping.ttl").into())?;
        let operators = translate_to_algebra(document);

        let output = File::create("op_trans_output.json")?;
        println!("{:#?}", operators);
        Ok(())
    }

    #[test]
    fn test_operator_translation_complex() -> ExtractorResult<()> {
        let document = parse_file(test_case!("multiple_tm.ttl").into())?;
        let operators = translate_to_algebra(document);

        let output = File::create("op_trans_complex_output.json")?;
        println!("{:#?}", operators);
        Ok(())
    }
}
