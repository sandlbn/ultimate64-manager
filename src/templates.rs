use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskTemplate {
    pub name: String,
    pub description: String,
    pub commands: Vec<String>,
}

impl std::fmt::Display for DiskTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

pub struct TemplateManager {
    templates: Vec<DiskTemplate>,
}

impl TemplateManager {
    pub fn new() -> Self {
        let mut manager = Self {
            templates: Vec::new(),
        };
        manager.load_default_templates();
        if let Err(e) = manager.load_custom_templates() {
            log::warn!("Could not load custom templates: {}", e);
        }
        manager
    }

    fn load_default_templates(&mut self) {
        self.templates = vec![
            DiskTemplate {
                name: "Load & Run First".to_string(),
                description: "Reset, load first program, and run".to_string(),
                commands: vec![
                    "RESET".to_string(),
                    "TYPE load\"*\",8,1\n".to_string(),
                    "TYPE run\n".to_string(),
                ],
            },
            DiskTemplate {
                name: "List Directory".to_string(),
                description: "Load and list disk directory".to_string(),
                commands: vec![
                    "RESET".to_string(),
                    "TYPE load\"$\",8\n".to_string(),
                    "TYPE list\n".to_string(),
                ],
            },
            DiskTemplate {
                name: "JiffyDOS Fast Load".to_string(),
                description: "Use JiffyDOS fast load".to_string(),
                commands: vec![
                    "RESET".to_string(),
                    "TYPE @8\n".to_string(),
                    "TYPE //*\n".to_string(),
                ],
            },
            DiskTemplate {
                name: "Reset Only".to_string(),
                description: "Just reset the machine".to_string(),
                commands: vec!["RESET".to_string()],
            },
            DiskTemplate {
                name: "Run".to_string(),
                description: "Just run an already loaded program".to_string(),
                commands: vec!["TYPE run\n".to_string()],
            },
        ];
    }

    fn load_custom_templates(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir()
            .ok_or("Could not determine config directory")?
            .join("ultimate64-manager")
            .join("templates");

        if !config_dir.exists() {
            return Ok(());
        }

        for entry in fs::read_dir(config_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                let contents = fs::read_to_string(&path)?;
                if let Ok(template) = serde_json::from_str::<DiskTemplate>(&contents) {
                    log::info!("Loaded custom template: {}", template.name);
                    self.templates.push(template);
                }
            }
        }

        Ok(())
    }

    pub fn get_templates(&self) -> Vec<DiskTemplate> {
        self.templates.clone()
    }

    #[allow(dead_code)]
    pub fn save_template(
        &mut self,
        template: DiskTemplate,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let config_dir = dirs::config_dir()
            .ok_or("Could not determine config directory")?
            .join("ultimate64-manager")
            .join("templates");

        fs::create_dir_all(&config_dir)?;

        let filename = format!("{}.json", template.name.to_lowercase().replace(' ', "_"));
        let filepath = config_dir.join(filename);

        let contents = serde_json::to_string_pretty(&template)?;
        fs::write(filepath, contents)?;

        self.templates.push(template);

        Ok(())
    }
}
