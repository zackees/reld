use anyhow::Result;
use minijinja::{context, Environment};

use crate::model::Artifact;

pub fn render(artifacts: &[Artifact], accepted: usize, rejected: usize) -> Result<String> {
    let mut environment = Environment::new();
    environment.add_template(
        "audit",
        "Artifact audit: {{ accepted }} accepted, {{ rejected }} rejected\n\
         {% for item in artifacts %}- {{ item.path }} [{{ item.kind }}] {{ item.bytes | length }} bytes\n{% endfor %}",
    )?;
    Ok(environment
        .get_template("audit")?
        .render(context!(artifacts => artifacts, accepted, rejected))?)
}
