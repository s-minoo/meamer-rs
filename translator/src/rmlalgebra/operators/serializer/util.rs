use std::collections::HashMap;

use crate::rmlalgebra::types::Quads;

pub fn unterminated_triple_strings(
    quad: &Quads<'_>,
    variable_map: &HashMap<String, String>,
) -> Vec<String> {
    let mut result: Vec<String> = vec![];
    let triples = &quad.triples;

    let sm = triples.sm;
    let sm_var = variable_map.get(&sm.tm_info.identifier).unwrap();

    let cls_templates = sm
        .classes
        .iter()
        .map(|cls| format!("{} a {}", sm_var, cls));
    result.extend(cls_templates);

    for pom in &triples.poms {
        let p_os = pom.pm.iter().flat_map(|pm| {
            let pm_var = variable_map.get(&pm.tm_info.identifier).unwrap();

            pom.om.iter().map(move |om| {
                let om_var = variable_map.get(&om.tm_info.identifier).unwrap();
                format!("{} {}", pm_var, om_var)
            })
        });

        let s_p_os = p_os.map(|p_o| format!("{} {}", sm_var, p_o));
        result.extend(s_p_os);
    }

    result
}
