use crate::Struct;
use bon::Builder;

#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct Section {
    pub name: String,
    pub title: String,
    pub summary: String,
    pub rules: Vec<String>,
}

impl Section {
    pub fn to_markdown(&self) -> String {
        let mut lines = vec![
            format!("# {} - {}", self.name, self.title),
            self.summary.clone(),
        ];
        for rule in self.rules.iter() {
            lines.push(format!("- {rule}"));
        }
        lines.join("\n")
    }

    pub fn to_xml(&self) -> String {
        let mut lines = Vec::default();
        lines.push(format!("<section name=\"{}\">", escape_xml(&self.name)));
        lines.push("  <title>".to_string());
        lines.push(format!("    {}", escape_xml(&self.title)));
        lines.push("  </title>".to_string());
        lines.push("  <summary>".to_string());
        lines.push(format!("    {}", escape_xml(&self.summary)));
        lines.push("  </summary>".to_string());
        lines.push("  <rules>".to_string());
        for rule in self.rules.iter() {
            lines.push(format!("    <rule>{}</rule>", escape_xml(rule)));
        }
        lines.push("  </rules>".to_string());
        lines.push("</section>".to_string());
        lines.join("\n")
    }
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct Prompt {
    pub sections: Vec<Section>,
}

impl Prompt {
    pub fn to_markdown(&self) -> String {
        self.sections
            .iter()
            .map(Section::to_markdown)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn to_xml(&self) -> String {
        self.sections
            .iter()
            .map(Section::to_xml)
            .collect::<Vec<_>>()
            .join("\n")
    }
}
