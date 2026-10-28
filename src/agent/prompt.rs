use crate::Struct;
use bon::Builder;

/// One prompt section rendered to markdown or XML.
#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct Section {
    /// Stable identifier used in headings and XML section names.
    pub name: String,
    /// Human-readable heading.
    pub title: String,
    /// One-paragraph orientation for the model.
    pub summary: String,
    /// Directive lines rendered as a list.
    pub rules: Vec<String>,
}

impl Section {
    /// Markdown rendering: heading, summary, then bulleted rules.
    pub fn to_markdown(&self) -> String {
        let mut lines = vec![
            format!("# {} - {}", self.name, self.title),
            self.summary.clone(),
        ];
        for rule in &self.rules {
            lines.push(format!("- {rule}"));
        }
        lines.join("\n")
    }

    /// XML rendering with escaped text content.
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
        for rule in &self.rules {
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

/// The system prompt as an ordered list of sections.
#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct Prompt {
    pub sections: Vec<Section>,
}

impl Prompt {
    /// Concatenates section markdown renderings.
    pub fn to_markdown(&self) -> String {
        self.sections
            .iter()
            .map(Section::to_markdown)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Concatenates section XML renderings.
    pub fn to_xml(&self) -> String {
        self.sections
            .iter()
            .map(Section::to_xml)
            .collect::<Vec<_>>()
            .join("\n")
    }
}
