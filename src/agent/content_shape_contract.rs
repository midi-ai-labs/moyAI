use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestTargetContentShapeContract {
    pub(crate) target: String,
    pub(crate) source_path: String,
    pub(crate) module_name: String,
    pub(crate) class_name: String,
}

pub(crate) fn python_source_for_test_target(
    target: &str,
) -> Option<TestTargetContentShapeContract> {
    let normalized = target.replace('\\', "/");
    let (dir, name) = normalized
        .rsplit_once('/')
        .map(|(dir, name)| (format!("{dir}/"), name.to_string()))
        .unwrap_or_else(|| (String::new(), normalized.clone()));
    let stem = name.strip_suffix(".py")?;
    let module = stem
        .strip_prefix("test_")
        .or_else(|| stem.strip_suffix("_test"))?;
    if module.trim().is_empty() {
        return None;
    }
    Some(TestTargetContentShapeContract {
        target: normalized,
        source_path: format!("{dir}{module}.py"),
        module_name: module.to_string(),
        class_name: format!("Test{}", snake_to_pascal(module)),
    })
}

impl TestTargetContentShapeContract {
    pub(crate) fn positive_shape_guidance(&self) -> String {
        format!(
            " Required positive test-module shape for `{target}`: import `unittest`; import `{module}` or public functions from `{source}`; define `{class_name}(unittest.TestCase)`; define one or more `def test_...` methods; assert behavior by calling `{module}.<public_function>(...)` or imported public functions. Optional launch block is allowed only as `if __name__ == \"__main__\": unittest.main()`. Forbidden shape: do not define production functions, do not define `main()`, do not call `input(...)`, and do not paste implementation code from `{source}`.",
            target = self.target,
            module = self.module_name,
            source = self.source_path,
            class_name = self.class_name
        )
    }

    pub(crate) fn prompt_contract(&self) -> String {
        format!(
            "Active write target contract:\n- Use the `write` tool with `path` set to `{target}` and `content` set to the complete replacement content for that file.\n- The provider-visible tool schema remains the stable `write` interface; target validation belongs to the tool lifecycle for the submitted call.\n- `{source}` is the inferred production source under test; do not rewrite `{source}` in this turn.\n- The `content` must be a complete test module for `{target}` only.\n- Required positive shape: import `unittest`; import `{module}` or public functions from `{source}`; define `{class_name}(unittest.TestCase)`; define one or more `def test_...` methods; assert requested behavior by calling `{module}.<public_function>(...)` or imported public functions.\n- Allowed launch block: `if __name__ == \"__main__\": unittest.main()`.\n- Forbidden shape: do not define production functions, do not define `main()`, do not call `input(...)`, and do not paste implementation code from `{source}`.\n- Older assistant narration, previous tool arguments, and prior progress output are not tool-call authority for this turn.",
            target = self.target,
            source = self.source_path,
            module = self.module_name,
            class_name = self.class_name
        )
    }

    pub(crate) fn tool_schema_description(&self) -> String {
        format!(
            "Complete final test module contents for `{target}`. Required positive shape: import `unittest`; import `{module}` or public functions from `{source}`; define `{class_name}(unittest.TestCase)`; define one or more `def test_...` methods; assert requested behavior by calling `{module}.<public_function>(...)` or imported public functions. Optional launch block may be `if __name__ == \"__main__\": unittest.main()`. `{source}` is the production source under test; do not send production source code, production functions, `def main()`, or `input(...)` for this test-target turn.",
            target = self.target,
            module = self.module_name,
            source = self.source_path,
            class_name = self.class_name
        )
    }

    pub(crate) fn metadata_json(&self) -> Value {
        json!({
            "kind": "python_test_module_content_shape",
            "target": self.target,
            "source_path": self.source_path,
            "module_name": self.module_name,
            "required_positive_shape": [
                "import unittest",
                format!("import {} or from {} import <public functions>", self.module_name, self.module_name),
                format!("class {}(unittest.TestCase)", self.class_name),
                "def test_<behavior>(self)",
                "assert requested behavior by calling the production module or imported public functions"
            ],
            "allowed_launch_block": "if __name__ == \"__main__\": unittest.main()",
            "forbidden_shape": [
                "production function definitions",
                "def main()",
                "input(...)",
                format!("pasted implementation code from {}", self.source_path)
            ]
        })
    }
}

pub(crate) fn test_target_content_shape_projection_is_positive_and_forbidden() -> bool {
    let Some(contract) = python_source_for_test_target("test_calculator.py") else {
        return false;
    };
    let prompt = contract.prompt_contract();
    let schema = contract.tool_schema_description();
    let guidance = contract.positive_shape_guidance();
    let metadata = contract.metadata_json();
    prompt.contains("Required positive shape")
        && prompt.contains("TestCalculator(unittest.TestCase)")
        && prompt.contains("Forbidden shape")
        && schema.contains("Required positive shape")
        && schema.contains("do not send production source code")
        && guidance.contains("def test_...")
        && guidance.contains("do not define `main()`")
        && metadata["kind"] == "python_test_module_content_shape"
        && metadata["module_name"] == "calculator"
}

fn snake_to_pascal(value: &str) -> String {
    value
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_ascii_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<String>()
}
