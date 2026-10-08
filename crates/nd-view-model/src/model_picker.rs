//! Directory-scoped model selection; stale replies cannot restore invalidated choices.
#[derive(Default)]
pub struct ModelPicker {
    choices: Vec<nd_wire::Model>,
    selected: Option<String>,
    cwd: Option<String>,
    generation: u64,
    loading: bool,
}
impl ModelPicker {
    pub fn invalidate(&mut self) {
        self.generation += 1;
        self.choices.clear();
        self.selected = None;
        self.cwd = None;
        self.loading = false;
    }
    pub fn begin(&mut self, cwd: String) -> u64 {
        self.invalidate();
        self.cwd = Some(cwd);
        self.loading = true;
        self.generation
    }
    pub fn finish(
        &mut self,
        generation: u64,
        result: Result<Vec<nd_wire::Model>, String>,
    ) -> Option<Result<(), String>> {
        if generation != self.generation {
            return None;
        }
        self.loading = false;
        Some(match result {
            Ok(choices) => {
                self.selected = choices
                    .iter()
                    .find(|m| !m.disabled)
                    .map(|m| m.value.clone());
                self.choices = choices;
                if self.selected.is_some() {
                    Ok(())
                } else {
                    Err("后端没有可用模型".into())
                }
            }
            Err(error) => Err(format!("获取模型失败：{error}")),
        })
    }
    pub fn choices(&self) -> &[nd_wire::Model] {
        &self.choices
    }
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }
    pub fn is_loading(&self) -> bool {
        self.loading
    }
    pub fn select(&mut self, value: &str) -> bool {
        if self.choices.iter().any(|m| m.value == value && !m.disabled) {
            self.selected = Some(value.into());
            true
        } else {
            false
        }
    }
    pub fn can_create(&self, cwd: &str) -> bool {
        !self.loading
            && self.cwd.as_deref() == Some(cwd)
            && self
                .choices
                .iter()
                .any(|m| Some(m.value.as_str()) == self.selected() && !m.disabled)
    }
}
