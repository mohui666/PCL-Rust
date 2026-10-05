//! A data-only renderer for PCL custom pages. Parsing never executes event handlers.
//! WPF CLR/object construction and code-behind are deliberately not interpreted.
use crate::{theme, ui_style};
use anyhow::{bail, Context, Result};
use eframe::egui::{self, Color32, RichText, Vec2};
use serde::Deserialize;
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

pub(super) const MAX_DOCUMENT: usize = 2 * 1024 * 1024;
#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct Node {
    pub(super) tag: String,
    #[serde(default)]
    pub(super) attrs: HashMap<String, String>,
    #[serde(default)]
    pub(super) children: Vec<Node>,
    #[serde(default)]
    pub(super) triggers: Vec<Node>,
}
impl Node {
    pub(super) fn attr(&self, key: &str) -> &str {
        self.attrs.get(key).map(String::as_str).unwrap_or("")
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Action {
    pub(super) kind: String,
    pub(super) data: String,
}

/// The origin controls relative images; a network page cannot reference local files.
#[derive(Clone, Debug, Default)]
pub(super) struct Origin {
    pub(super) directory: Option<PathBuf>,
    pub(super) url: Option<String>,
}

pub(super) fn http_url(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).context("网址格式无效")?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("仅支持不含用户名和密码的 HTTP(S) 网址");
    }
    Ok(url)
}
pub(super) fn fetch_bytes(url: &str, maximum: usize) -> Result<Vec<u8>> {
    let url = http_url(url)?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("重定向次数过多")
            } else if http_url(attempt.url().as_str()).is_err() {
                attempt.error("重定向不是 HTTP(S) 网址")
            } else {
                attempt.follow()
            }
        }))
        .user_agent(concat!("PCL-Rust/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let response = client.get(url).send()?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|len| len > maximum as u64)
    {
        bail!("内容超过 {} MiB", maximum / 1024 / 1024);
    }
    let mut bytes = Vec::new();
    response.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        bail!("内容超过 {} MiB", maximum / 1024 / 1024);
    }
    Ok(bytes)
}

pub(super) fn parse(text: &str) -> Result<Vec<Node>> {
    if text.len() > MAX_DOCUMENT {
        bail!("主页内容超过 2 MiB");
    }
    if text.contains("<!DOCTYPE") || text.contains("<!ENTITY") {
        bail!("主页不支持 DTD 或外部实体");
    }
    let text = text.trim_start_matches('\u{feff}').trim();
    let text = if text.starts_with("<?xml") {
        text.split_once("?>").context("XML 声明未结束")?.1
    } else {
        text
    };
    let wrapped=format!("<Root xmlns=\"http://schemas.microsoft.com/winfx/2006/xaml/presentation\" xmlns:x=\"http://schemas.microsoft.com/winfx/2006/xaml\" xmlns:local=\"clr-namespace:PCL\">{text}</Root>");
    let document = roxmltree::Document::parse(&wrapped).context("主页 XAML 格式有误")?;
    let mut count = 0;
    let raw: Vec<Node> = document
        .root_element()
        .children()
        .filter(|node| node.is_element())
        .map(|node| parse_node(node, 0, &mut count))
        .collect::<Result<_>>()?;
    let mut expanded = 0;
    raw.into_iter()
        .map(|node| materialize(node, &StaticResources::default(), 0, &mut expanded))
        .collect()
}
fn parse_node(node: roxmltree::Node<'_, '_>, depth: usize, count: &mut usize) -> Result<Node> {
    *count += 1;
    if depth > 40 || *count > 4000 {
        bail!("主页布局超过 40 层或 4000 个元素");
    }
    let tag = node.tag_name().name();
    if !matches!(
        tag,
        "StackPanel"
            | "WrapPanel"
            | "Grid"
            | "Border"
            | "DockPanel"
            | "ScrollViewer"
            | "MyScrollViewer"
            | "TextBlock"
            | "Label"
            | "Run"
            | "Span"
            | "Bold"
            | "Italic"
            | "Underline"
            | "LineBreak"
            | "Hyperlink"
            | "Image"
            | "MyImage"
            | "MyCard"
            | "MyHint"
            | "MyButton"
            | "MyTextButton"
            | "MyIconButton"
            | "MyIconTextButton"
            | "MyListItem"
            | "Path"
            | "Rectangle"
            | "Grid.RowDefinitions"
            | "Grid.ColumnDefinitions"
            | "RowDefinition"
            | "ColumnDefinition"
            | "CustomEventService.Events"
            | "CustomEventCollection"
            | "CustomEvent"
            | "MyCheckBox"
            | "MyTextBox"
            | "MyComboBox"
            | "MyComboBoxItem"
            | "Trigger"
            | "DataTrigger"
            | "Style.Triggers"
            | "TextBlock.Triggers"
            | "MyButton.Triggers"
            | "MyCheckBox.Triggers"
            | "StackPanel.Triggers"
            | "Grid.Triggers"
            | "Border.Triggers"
            | "StackPanel.Resources"
            | "Grid.Resources"
            | "Border.Resources"
            | "FlowDocument.Resources"
            | "Style"
            | "Setter"
            | "String"
            | "ControlTemplate"
            | "ContentControl"
            | "FlowDocument"
            | "FlowDocumentScrollViewer"
            | "Paragraph"
            | "List"
            | "ListItem"
            | "Section"
            | "MyLoading"
            | "Line"
    ) {
        bail!("暂不支持的 XAML 元素：{tag}（未执行此内容）");
    }
    let mut attrs = HashMap::new();
    for attr in node.attributes() {
        let name = attr.name();
        if matches!(
            name,
            "Loaded" | "Initialized" | "Unloaded" | "DataContext" | "Class" | "FactoryMethod"
        ) {
            bail!("不支持自动执行或对象构造属性：{name}");
        }
        if attr.value().starts_with("{Binding") {
            parse_binding(attr.value())?;
            if name == "Source" {
                bail!("图片地址不能绑定输入或私有变量");
            }
        }
        attrs.insert(
            name.strip_prefix("CustomEventService.")
                .unwrap_or(name)
                .to_owned(),
            attr.value().to_owned(),
        );
    }
    let mut children = Vec::new();
    for child in node.children() {
        if child.is_element() {
            children.push(parse_node(child, depth + 1, count)?);
        } else if child.is_text()
            && matches!(
                tag,
                "TextBlock"
                    | "Label"
                    | "Run"
                    | "Span"
                    | "Bold"
                    | "Italic"
                    | "Underline"
                    | "Hyperlink"
                    | "Paragraph"
                    | "ListItem"
                    | "String"
                    | "MyTextButton"
                    | "MyButton"
            )
        {
            if let Some(text) = child.text().filter(|text| !text.trim().is_empty()) {
                children.push(Node {
                    tag: "Run".into(),
                    attrs: HashMap::from([("Text".into(), text.into())]),
                    children: vec![],
                    triggers: vec![],
                });
            }
        }
    }
    let mut triggers = Vec::new();
    children.retain(|child| {
        if child.tag.ends_with(".Triggers") {
            triggers.extend(child.children.clone());
            false
        } else {
            true
        }
    });
    for trigger in &triggers {
        validate_trigger(trigger)?;
    }
    Ok(Node {
        tag: tag.into(),
        attrs,
        children,
        triggers,
    })
}

#[derive(Debug)]
struct Binding {
    path: String,
    element: Option<String>,
    fallback: String,
}
fn parse_binding(text: &str) -> Result<Binding> {
    let body = text
        .strip_prefix("{Binding")
        .and_then(|s| s.strip_suffix('}'))
        .context("Binding 格式无效")?
        .trim();
    let mut binding = Binding {
        path: String::new(),
        element: None,
        fallback: String::new(),
    };
    for (index, part) in body.split(',').map(str::trim).enumerate() {
        if part.is_empty() {
            continue;
        }
        if let Some((key, value)) = part.split_once('=') {
            let value = value.trim();
            match key.trim() {
                "Path" => binding.path = value.into(),
                "ElementName" => binding.element = Some(value.into()),
                "FallbackValue" => binding.fallback = value.into(),
                "Mode" if value == "OneWay" => (),
                _ => bail!("Binding 仅支持 Path、ElementName、FallbackValue 与单向模式"),
            }
        } else if index == 0 {
            binding.path = part.into();
        } else {
            bail!("Binding 参数无效");
        }
    }
    if binding.path.len() > 128
        || binding
            .element
            .as_ref()
            .is_some_and(|name| name.len() > 128)
    {
        bail!("Binding 名称过长");
    }
    Ok(binding)
}
fn validate_trigger(trigger: &Node) -> Result<()> {
    match trigger.tag.as_str() {
        "Trigger"
            if matches!(
                trigger.attr("Property"),
                "IsMouseOver"
                    | "IsPressed"
                    | "IsChecked"
                    | "Checked"
                    | "Text"
                    | "SelectedItem"
                    | "IsEnabled"
            ) => {}
        "DataTrigger" => {
            parse_binding(trigger.attr("Binding"))?;
        }
        _ => bail!("仅支持本地控件状态 Trigger 与只读 DataTrigger；不执行自动事件"),
    }
    for setter in &trigger.children {
        if setter.tag != "Setter"
            || !setter.children.is_empty()
            || !setter.triggers.is_empty()
            || !setter.attr("TargetName").is_empty()
            || !matches!(
                setter.attr("Property"),
                "Visibility"
                    | "Foreground"
                    | "Background"
                    | "Opacity"
                    | "Text"
                    | "Content"
                    | "IsEnabled"
                    | "Width"
                    | "Height"
                    | "FontSize"
            )
        {
            bail!("Trigger 只能修改自身的显示、文本、颜色或尺寸；不能执行事件、读取文件或联网");
        }
    }
    Ok(())
}

#[derive(Clone, Default)]
struct StaticResources {
    implicit: HashMap<String, HashMap<String, String>>,
    named: HashMap<String, HashMap<String, String>>,
    implicit_triggers: HashMap<String, Vec<Node>>,
    named_triggers: HashMap<String, Vec<Node>>,
    strings: HashMap<String, String>,
    templates: HashMap<String, Vec<Node>>,
    text: HashMap<String, String>,
}
fn resource_name(value: &str, kind: &str) -> Option<String> {
    value
        .strip_prefix(&format!("{{{kind} "))
        .and_then(|s| s.strip_suffix('}'))
        .map(|s| s.trim().to_owned())
}
fn materialize(
    mut node: Node,
    inherited: &StaticResources,
    depth: usize,
    count: &mut usize,
) -> Result<Node> {
    *count += 1;
    if depth > 40 || *count > 4000 {
        bail!("主页静态资源展开超过安全大小限制");
    }
    let mut resources = inherited.clone();
    for block in node
        .children
        .iter()
        .filter(|node| node.tag.ends_with(".Resources"))
    {
        for entry in &block.children {
            match entry.tag.as_str() {
                "String" => {
                    resources
                        .strings
                        .insert(entry.attr("Key").into(), plain_text(entry));
                }
                "Style" => {
                    if !entry.attr("BasedOn").is_empty() {
                        bail!("暂不支持 Style.BasedOn 继承");
                    }
                    let mut attrs = HashMap::new();
                    for setter in &entry.children {
                        if setter.tag != "Setter" || !setter.children.is_empty() {
                            bail!("样式仅支持静态 Setter 值");
                        }
                        let property = setter.attr("Property");
                        if matches!(
                            property,
                            "Loaded"
                                | "Initialized"
                                | "Unloaded"
                                | "DataContext"
                                | "Class"
                                | "FactoryMethod"
                        ) {
                            bail!("样式不能声明自动执行/对象构造属性");
                        }
                        attrs.insert(property.into(), setter.attr("Value").into());
                    }
                    if entry.attr("Key").is_empty() {
                        let target = entry
                            .attr("TargetType")
                            .strip_prefix("local:")
                            .unwrap_or(entry.attr("TargetType"));
                        resources
                            .implicit_triggers
                            .insert(target.into(), entry.triggers.clone());
                        resources.implicit.insert(target.into(), attrs);
                    } else {
                        resources
                            .named_triggers
                            .insert(entry.attr("Key").into(), entry.triggers.clone());
                        resources.named.insert(entry.attr("Key").into(), attrs);
                    }
                }
                "ControlTemplate" => {
                    resources
                        .templates
                        .insert(entry.attr("Key").into(), entry.children.clone());
                }
                _ => bail!("资源字典仅支持静态 Style、String 与 ControlTemplate"),
            }
        }
    }
    let mut inherited_triggers = resources
        .implicit_triggers
        .get(&node.tag)
        .cloned()
        .unwrap_or_default();
    let mut attrs = resources
        .implicit
        .get(&node.tag)
        .cloned()
        .unwrap_or_default();
    if let Some(name) = resource_name(node.attr("Style"), "StaticResource") {
        inherited_triggers.extend(
            resources
                .named_triggers
                .get(&name)
                .cloned()
                .unwrap_or_default(),
        );
        attrs.extend(
            resources
                .named
                .get(&name)
                .with_context(|| format!("未找到静态样式：{name}"))?
                .clone(),
        );
    }
    inherited_triggers.append(&mut node.triggers);
    node.triggers = inherited_triggers;
    attrs.extend(node.attrs);
    for (name, value) in &resources.text {
        attrs.entry(name.clone()).or_insert_with(|| value.clone());
    }
    for name in [
        "FontSize",
        "FontWeight",
        "FontStyle",
        "Foreground",
        "FontFamily",
    ] {
        if let Some(value) = attrs.get(name) {
            resources.text.insert(name.into(), value.clone());
        }
    }
    for value in attrs.values_mut() {
        if let Some(name) = resource_name(value, "StaticResource") {
            if let Some(text) = resources.strings.get(&name) {
                *value = text.clone();
            }
        }
    }
    if attrs
        .get("Source")
        .is_some_and(|value| value.starts_with("{Binding"))
    {
        bail!("图片地址不能绑定输入或私有变量");
    }
    node.attrs = attrs;
    if node.tag == "ContentControl" {
        if let Some(name) = resource_name(node.attr("Template"), "StaticResource") {
            node.children = resources
                .templates
                .get(&name)
                .with_context(|| format!("未找到静态模板：{name}"))?
                .clone();
            fn bind(nodes: &mut [Node], attrs: &HashMap<String, String>) {
                for node in nodes {
                    for value in node.attrs.values_mut() {
                        if let Some(name) = resource_name(value, "TemplateBinding") {
                            *value = attrs.get(&name).cloned().unwrap_or_default();
                        }
                    }
                    bind(&mut node.children, attrs);
                }
            }
            bind(&mut node.children, &node.attrs);
        }
    }
    node.children = node
        .children
        .into_iter()
        .filter(|node| !node.tag.ends_with(".Resources") && !node.tag.ends_with(".Triggers"))
        .map(|node| materialize(node, &resources, depth + 1, count))
        .collect::<Result<_>>()?;
    Ok(node)
}
fn plain_text(node: &Node) -> String {
    let mut text = node.attr("Text").to_owned();
    for child in &node.children {
        text.push_str(&plain_text(child));
    }
    text
}

pub(super) fn replace(text: &str, values: &HashMap<String, String>) -> String {
    let mut result = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        let candidate = &rest[start + 1..];
        let Some(end) = candidate.find('}') else {
            result.push_str(&rest[start..]);
            return result;
        };
        let key = &candidate[..end];
        if let Some(value) = values.get(key) {
            result.push_str(value);
        } else if let Some(variable) = key
            .strip_prefix("variable:")
            .or_else(|| key.strip_prefix("varible:"))
        {
            let (name, default) = variable.split_once(':').unwrap_or((variable, ""));
            result.push_str(
                values
                    .get(&format!("variable:{name}"))
                    .map(String::as_str)
                    .unwrap_or(default),
            );
        } else {
            result.push_str(&rest[start..start + end + 2]);
        }
        rest = &candidate[end + 1..];
    }
    result.push_str(rest);
    result
}

#[derive(Default)]
pub(super) struct Renderer {
    images: HashMap<String, Picture>,
    inputs: HashMap<String, String>,
    checks: HashMap<String, bool>,
    bound_values: HashMap<String, String>,
}
enum Picture {
    Pending(mpsc::Receiver<Result<egui::ColorImage>>),
    Ready(egui::TextureHandle),
    Error(String),
}
impl Renderer {
    fn binding_value(&self, text: &str, values: &HashMap<String, String>) -> String {
        let Ok(binding) = parse_binding(text) else {
            return replace(text, values);
        };
        if let Some(element) = binding.element {
            match binding.path.as_str() {
                "Text" | "Content" | "SelectedItem" => self
                    .inputs
                    .get(&element)
                    .cloned()
                    .unwrap_or(binding.fallback),
                "IsChecked" | "Checked" => self
                    .checks
                    .get(&element)
                    .map(|checked| if *checked { "True" } else { "False" }.into())
                    .unwrap_or(binding.fallback),
                _ => binding.fallback,
            }
        } else {
            values
                .get(&binding.path)
                .cloned()
                .unwrap_or(binding.fallback)
        }
    }
    fn resolved_node(
        &self,
        node: &Node,
        values: &HashMap<String, String>,
        hovered: bool,
        pressed: bool,
    ) -> Node {
        let mut resolved = node.clone();
        for property in ["Text", "Checked"] {
            if node.attr(property).starts_with("{Binding") {
                resolved
                    .attrs
                    .insert(format!("_PclBinding{property}"), "True".into());
            }
        }
        for value in resolved.attrs.values_mut() {
            if value.starts_with("{Binding") {
                *value = self.binding_value(value, values);
            }
        }
        for trigger in &node.triggers {
            let actual = if trigger.tag == "DataTrigger" {
                self.binding_value(trigger.attr("Binding"), values)
            } else {
                match trigger.attr("Property") {
                    "IsMouseOver" => if hovered { "True" } else { "False" }.into(),
                    "IsPressed" => if pressed { "True" } else { "False" }.into(),
                    "IsChecked" | "Checked" => self
                        .checks
                        .get(node.attr("Name"))
                        .copied()
                        .unwrap_or(node.attr("Checked") == "True")
                        .to_string(),
                    "Text" | "SelectedItem" => self
                        .inputs
                        .get(node.attr("Name"))
                        .cloned()
                        .unwrap_or_else(|| node.attr("Text").into()),
                    "IsEnabled" => {
                        if resolved.attr("IsEnabled").is_empty() {
                            "True".into()
                        } else {
                            resolved.attr("IsEnabled").into()
                        }
                    }
                    property => resolved.attr(property).into(),
                }
            };
            if actual.eq_ignore_ascii_case(trigger.attr("Value")) {
                for setter in &trigger.children {
                    resolved.attrs.insert(
                        setter.attr("Property").into(),
                        self.binding_value(setter.attr("Value"), values),
                    );
                }
            }
        }
        resolved
    }
    fn seed_named_inputs(&mut self, nodes: &[Node], values: &HashMap<String, String>) {
        for node in nodes {
            if !node.attr("Name").is_empty() {
                if matches!(node.tag.as_str(), "MyTextBox" | "MyComboBox") {
                    self.inputs
                        .entry(node.attr("Name").into())
                        .or_insert_with(|| replace(node.attr("Text"), values));
                }
                if node.tag == "MyCheckBox" {
                    self.checks
                        .entry(node.attr("Name").into())
                        .or_insert(node.attr("Checked") == "True");
                }
            }
            self.seed_named_inputs(&node.children, values);
        }
    }
    pub(super) fn clear(&mut self) {
        self.images.clear();
        self.inputs.clear();
        self.checks.clear();
        self.bound_values.clear();
    }
    pub(super) fn render(
        &mut self,
        ui: &mut egui::Ui,
        nodes: &[Node],
        origin: &Origin,
        values: &HashMap<String, String>,
    ) -> Vec<Action> {
        self.seed_named_inputs(nodes, values);
        let mut actions = Vec::new();
        for (index, node) in nodes.iter().enumerate() {
            ui.push_id(index, |ui| {
                self.node(ui, node, origin, values, &mut actions)
            });
        }
        if let Some(base) = origin.url.as_deref().and_then(|url| http_url(url).ok()) {
            for action in &mut actions {
                if action.kind == "打开帮助" && !action.data.contains("://") {
                    if let Ok(url) = base.join(&action.data) {
                        action.data = url.to_string();
                    }
                }
            }
        }
        actions
    }
    fn node(
        &mut self,
        ui: &mut egui::Ui,
        node: &Node,
        origin: &Origin,
        values: &HashMap<String, String>,
        actions: &mut Vec<Action>,
    ) {
        let region = ui.id().with("xaml-node-region");
        let hovered = ui
            .ctx()
            .data(|data| data.get_temp::<egui::Rect>(region))
            .is_some_and(|rect| ui.rect_contains_pointer(rect));
        let resolved = self.resolved_node(
            node,
            values,
            hovered,
            hovered && ui.input(|input| input.pointer.primary_down()),
        );
        let node = &resolved;
        let value = |key: &str| replace(node.attr(key), values);
        if matches!(value("Visibility").as_str(), "Collapsed" | "Hidden") {
            return;
        }
        let margin = edges(node.attr("Margin"));
        ui.add_space(margin[1].max(0.0));
        let rendered=egui::Frame::NONE.fill(brush(&value("Background"),ui.ctx()).unwrap_or(Color32::TRANSPARENT)).inner_margin(egui::Margin{left:margin[0].clamp(0.0,127.0) as i8,right:margin[2].clamp(0.0,127.0) as i8,top:0,bottom:0}).show(ui,|ui| {
            if node.attr("IsEnabled")=="False"{ui.disable();}
            if let Some(opacity)=number(node.attr("Opacity")) {ui.multiply_opacity(opacity.clamp(0.0,1.0));}
            if let Some(width)=number(node.attr("Width")) {ui.set_width(width.min(ui.available_width()));}
            if let Some(height)=number(node.attr("Height")).or_else(||number(node.attr("MinHeight"))) {ui.set_min_height(height);}
            match node.tag.as_str() {
                "MyCard"=>self.card(ui,node,origin,values,actions),
                "TextBlock"|"Label"|"Run"|"Span"|"Bold"|"Italic"|"Underline"|"Hyperlink"|"Paragraph"|"ListItem"=> {
                    let mut job=egui::text::LayoutJob::default();job.wrap.max_width=ui.available_width();
                    let format=egui::TextFormat{font_id:egui::FontId::proportional(number(node.attr("FontSize")).unwrap_or(13.0).clamp(6.0,96.0)),color:brush(&value("Foreground"),ui.ctx()).unwrap_or(theme::palette(ui.ctx()).text),..Default::default()};
                    if inline_actions(node) {
                        ui.horizontal_wrapped(|ui| self.inline(ui,node,&format,origin,values,actions));
                        return;
                    }
                    text_job(node,&format,values,&mut job);
                    let response=ui.add(egui::Label::new(job).wrap().selectable(true));
                    if node.tag=="Hyperlink" && response.interact(egui::Sense::click()).clicked() {actions.push(Action{kind:"打开网页".into(),data:value("NavigateUri")});}
                }
                "MyHint"=> {egui::Frame::NONE.fill(if matches!(node.attr("Theme"),"Blue"|"Green") {theme::palette(ui.ctx()).light} else {Color32::from_rgb(255,241,223)}).inner_margin(10).corner_radius(3).show(ui,|ui| {let response=ui.add(egui::Label::new(value("Text")).wrap());if response.interact(egui::Sense::click()).clicked(){collect_actions(node,values,actions);}});}
                "MyButton"|"MyTextButton"|"MyIconButton"|"MyIconTextButton"|"MyListItem"=> {
                    let text=if node.attr("Text").is_empty(){if node.attr("Title").is_empty(){value("Content")}else{value("Title")}}else{value("Text")};
                    let text=if text.is_empty(){let content=plain_text(node);if content.is_empty(){"↻".into()}else{content}}else{text};
                    let height=number(node.attr("Height")).unwrap_or(if node.tag=="MyListItem" {42.0}else{35.0});
                    let width=number(node.attr("Width")).or_else(||number(node.attr("MinWidth"))).unwrap_or(if node.tag=="MyListItem"{ui.available_width()}else{140.0});
                    let response=if node.tag=="MyTextButton" {
                        ui.add_enabled(node.attr("IsEnabled")!="False",egui::Button::new(RichText::new(text).size(number(node.attr("FontSize")).unwrap_or(14.0)).color(theme::palette(ui.ctx()).accent)).frame(false).min_size(Vec2::new(0.0,18.0)))
                    }else{ui.add_enabled(node.attr("IsEnabled")!="False",egui::Button::new(text).min_size(Vec2::new(width.min(ui.available_width()),height)))};
                    if response.clicked(){collect_actions(node,values,actions);}
                    if !node.attr("ToolTip").is_empty(){response.on_hover_text(value("ToolTip"));}
                    if !node.attr("Info").is_empty(){ui.label(RichText::new(value("Info")).size(12.0));}
                }
                "Image"|"MyImage"=>{
                    // Automatic network requests do not carry private paths or variables.
                    let source=if origin.url.is_some() || node.attr("Source").starts_with("http") {
                        replace(node.attr("Source"),&public_markers(values))
                    }else{value("Source")};
                    self.image(ui,&source,node,origin);
                },
                "StackPanel" if node.attr("Orientation")=="Horizontal"=> {ui.horizontal(|ui|self.children(ui,node,origin,values,actions));}
                "WrapPanel"=>{ui.horizontal_wrapped(|ui|self.children(ui,node,origin,values,actions));}
                "DockPanel"=>self.dock(ui,node,origin,values,actions),
                "Grid"=>self.grid(ui,node,origin,values,actions),
                "Border"=>{egui::Frame::NONE.fill(brush(&value("Background"),ui.ctx()).unwrap_or(Color32::TRANSPARENT)).corner_radius(number(node.attr("CornerRadius")).unwrap_or(0.0) as u8).inner_margin(number(node.attr("Padding")).unwrap_or(0.0) as i8).show(ui,|ui|self.children(ui,node,origin,values,actions));}
                "ScrollViewer"|"MyScrollViewer"|"FlowDocumentScrollViewer"=>{egui::ScrollArea::vertical().id_salt(ui.id().with("xaml-scroll")).max_height(number(node.attr("Height")).unwrap_or(400.0)).show(ui,|ui|self.children(ui,node,origin,values,actions));}
                "MyTextBox"=>{
                    let key=if node.attr("Name").is_empty(){format!("{:?}",ui.id())}else{node.attr("Name").into()};
                    let text=value("Text");
                    if node.attr("_PclBindingText")=="True" && self.bound_values.get(&key)!=Some(&text) {
                        self.bound_values.insert(key.clone(),text.clone());self.inputs.insert(key.clone(),text.clone());
                    }
                    let input=self.inputs.entry(key).or_insert(text);
                    ui.add_sized([number(node.attr("Width")).unwrap_or(ui.available_width()),28.0],egui::TextEdit::singleline(input));
                }
                "MyCheckBox"=>{
                    let key=if node.attr("Name").is_empty(){format!("{:?}",ui.id())}else{node.attr("Name").into()};
                    let text=value("Checked");
                    if node.attr("_PclBindingChecked")=="True" && self.bound_values.get(&key)!=Some(&text) {
                        self.bound_values.insert(key.clone(),text.clone());self.checks.insert(key.clone(),text=="True");
                    }
                    let checked=self.checks.entry(key).or_insert(text=="True");
                    if ui_style::checkbox(ui,checked,&value("Text"),"").changed(){collect_actions(node,values,actions);}
                }
                "MyComboBox"=>{
                    let key=if node.attr("Name").is_empty(){format!("{:?}",ui.id())}else{node.attr("Name").into()};
                    let text=value("Text");
                    if node.attr("_PclBindingText")=="True" && self.bound_values.get(&key)!=Some(&text) {
                        self.bound_values.insert(key.clone(),text.clone());self.inputs.insert(key.clone(),text);
                    }
                    let selected=self.inputs.entry(key).or_default();
                    ui_style::PclComboBox::from_id_salt(ui.id()).width(ui.available_width()).selected_text(selected.clone()).show_ui(ui,|ui|{
                        for item in &node.children {let text=replace(item.attr("Content"),values);if ui.selectable_value(selected,text.clone(),text).clicked(){collect_actions(item,values,actions);}}
                    });
                }
                "MyLoading"=>{super::loading_ui::control(ui, &value("Text"),Vec2::new(number(node.attr("Width")).unwrap_or(ui.available_width()),number(node.attr("Height")).unwrap_or(77.0)));}
                "Line"=>{let width=number(node.attr("Width")).unwrap_or(ui.available_width());let thickness=number(node.attr("StrokeThickness")).unwrap_or(1.0);let(rect,_)=ui.allocate_exact_size(Vec2::new(width,thickness.max(2.0)),egui::Sense::hover());ui.painter().line_segment([rect.left_center(),rect.right_center()],egui::Stroke::new(thickness,brush(&value("Stroke"),ui.ctx()).unwrap_or(theme::palette(ui.ctx()).border)));}
                "Rectangle"=>{let (rect,_)=ui.allocate_exact_size(Vec2::new(number(node.attr("Width")).unwrap_or(ui.available_width()),number(node.attr("Height")).unwrap_or(1.0)),egui::Sense::hover());ui.painter().rect_filled(rect,0,brush(&value("Fill"),ui.ctx()).unwrap_or(theme::palette(ui.ctx()).light));}
                "Path"=>{
                    let data=value("Data"); let color=brush(&value("Fill"),ui.ctx()).unwrap_or(theme::palette(ui.ctx()).text);
                    if !data.is_empty(){let source=format!("<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 48 48'><path d=\"{}\" fill='white'/></svg>",escape_xml(&data));self.svg(ui,&source,Vec2::new(number(node.attr("Width")).unwrap_or(24.0),number(node.attr("Height")).unwrap_or(24.0)),color);}
                }
                "Grid.RowDefinitions"|"Grid.ColumnDefinitions"|"RowDefinition"|"ColumnDefinition"|"CustomEventService.Events"|"CustomEventCollection"|"CustomEvent"=>(),
                _=>self.children(ui,node,origin,values,actions),
            }
        });
        ui.ctx()
            .data_mut(|data| data.insert_temp(region, rendered.response.rect));
        ui.add_space(margin[3].max(0.0));
    }
    fn inline(
        &mut self,
        ui: &mut egui::Ui,
        node: &Node,
        format: &egui::TextFormat,
        origin: &Origin,
        values: &HashMap<String, String>,
        actions: &mut Vec<Action>,
    ) {
        if matches!(
            node.tag.as_str(),
            "MyTextButton" | "MyButton" | "MyIconTextButton" | "Hyperlink"
        ) {
            self.node(ui, node, origin, values, actions);
            return;
        }
        let mut format = format.clone();
        if node.tag == "Bold" {
            format.font_id.family = egui::FontFamily::Name("PCL Bold".into());
        }
        if node.tag == "Italic" {
            format.italics = true;
        }
        if node.tag == "Underline" {
            format.underline = egui::Stroke::new(1.0_f32, format.color);
        }
        if !node.attr("Text").is_empty() {
            let mut job = egui::text::LayoutJob::default();
            job.append(&replace(node.attr("Text"), values), 0.0, format.clone());
            ui.add(egui::Label::new(job).wrap());
        }
        for child in &node.children {
            self.inline(ui, child, &format, origin, values, actions);
        }
    }
    fn children(
        &mut self,
        ui: &mut egui::Ui,
        node: &Node,
        origin: &Origin,
        values: &HashMap<String, String>,
        actions: &mut Vec<Action>,
    ) {
        for (index, child) in node.children.iter().enumerate() {
            ui.push_id(index, |ui| self.node(ui, child, origin, values, actions));
        }
    }
    fn card(
        &mut self,
        ui: &mut egui::Ui,
        node: &Node,
        origin: &Origin,
        values: &HashMap<String, String>,
        actions: &mut Vec<Action>,
    ) {
        let can_swap = node.attr("CanSwap") == "True";
        let id = ui.id().with("xaml-card");
        let mut open = ui.data_mut(|data| {
            *data.get_temp_mut_or_insert_with(id, || node.attr("IsSwapped") != "True")
        });
        let start = ui.next_widget_position();
        let width = ui.available_width();
        let painter = ui.painter().clone();
        let shadow = painter.add(egui::Shape::Noop);
        let background = painter.add(egui::Shape::Noop);
        let mut height = 40.0_f32;
        if open {
            let mut content =
                ui.new_child(egui::UiBuilder::new().id_salt("card-content").max_rect(
                    egui::Rect::from_min_size(
                        start,
                        Vec2::new(width, ui.available_height().max(1000.0)),
                    ),
                ));
            content.spacing_mut().item_spacing.y = 0.0;
            if node
                .children
                .iter()
                .any(|child| !child.attr("Margin").is_empty())
            {
                // MyCard itself is an overlay Grid. Raw XAML positions its own body.
                self.children(&mut content, node, origin, values, actions);
            } else {
                // The bundled help catalog predates the renderer and strips layout margins.
                content.add_space(40.0);
                egui::Frame::NONE
                    .inner_margin(egui::Margin {
                        left: 25,
                        right: 23,
                        top: 0,
                        bottom: 15,
                    })
                    .show(&mut content, |ui| {
                        self.children(ui, node, origin, values, actions)
                    });
            }
            height = height.max(content.min_rect().bottom() - start.y);
        }
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
        painter.set(
            shadow,
            egui::epaint::Shadow {
                offset: [0, 2],
                blur: 3,
                spread: 0,
                color: Color32::from_black_alpha(9),
            }
            .as_shape(rect, 5),
        );
        painter.set(
            background,
            egui::Shape::rect_filled(rect, 5, Color32::from_white_alpha(245)),
        );
        painter.text(
            rect.min + Vec2::new(15.0, 12.0),
            egui::Align2::LEFT_TOP,
            replace(node.attr("Title"), values),
            egui::FontId::proportional(13.0),
            theme::palette(ui.ctx()).text,
        );
        if can_swap {
            let header = egui::Rect::from_min_size(rect.min, Vec2::new(width, 40.0));
            let response = ui.interact(header, id, egui::Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::CollapsingHeader,
                    true,
                    open,
                    replace(node.attr("Title"), values),
                )
            });
            let center = egui::pos2(rect.right() - 21.0, rect.top() + 20.0);
            let sign = if open { 1.0 } else { -1.0 };
            painter.add(egui::Shape::line(
                vec![
                    center + Vec2::new(-4.0, -2.0 * sign),
                    center + Vec2::new(0.0, 2.0 * sign),
                    center + Vec2::new(4.0, -2.0 * sign),
                ],
                egui::Stroke::new(1.5_f32, theme::palette(ui.ctx()).text),
            ));
            if response.clicked() {
                open = !open;
                ui.data_mut(|data| data.insert_temp(id, open));
            }
        }
    }
    fn dock(
        &mut self,
        ui: &mut egui::Ui,
        node: &Node,
        origin: &Origin,
        values: &HashMap<String, String>,
        actions: &mut Vec<Action>,
    ) {
        let start = ui.next_widget_position();
        let total = Vec2::new(
            number(node.attr("Width"))
                .unwrap_or(ui.available_width())
                .min(ui.available_width()),
            number(node.attr("Height")).unwrap_or(240.0),
        );
        let mut remaining = egui::Rect::from_min_size(start, total);
        for (index, child) in node.children.iter().enumerate() {
            let fill = index + 1 == node.children.len() && node.attr("LastChildFill") != "False";
            let dock = child.attr("DockPanel.Dock");
            let width = number(child.attr("Width"))
                .unwrap_or(140.0)
                .min(remaining.width());
            let height = number(child.attr("Height"))
                .unwrap_or(35.0)
                .min(remaining.height());
            let mut rect = remaining;
            if !fill {
                match dock {
                    "Right" => {
                        rect.min.x = rect.max.x - width;
                        remaining.max.x = rect.min.x;
                    }
                    "Top" => {
                        rect.max.y = rect.min.y + height;
                        remaining.min.y = rect.max.y;
                    }
                    "Bottom" => {
                        rect.min.y = rect.max.y - height;
                        remaining.max.y = rect.min.y;
                    }
                    _ => {
                        rect.max.x = rect.min.x + width;
                        remaining.min.x = rect.max.x;
                    }
                }
            }
            let mut child_ui = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("dock", index))
                    .max_rect(rect),
            );
            child_ui.set_clip_rect(ui.clip_rect().intersect(rect));
            let mut placed = child.clone();
            placed
                .attrs
                .entry("Width".into())
                .or_insert_with(|| rect.width().to_string());
            placed
                .attrs
                .entry("Height".into())
                .or_insert_with(|| rect.height().to_string());
            self.node(&mut child_ui, &placed, origin, values, actions);
        }
        ui.allocate_exact_size(total, egui::Sense::hover());
    }
    fn grid(
        &mut self,
        ui: &mut egui::Ui,
        node: &Node,
        origin: &Origin,
        values: &HashMap<String, String>,
        actions: &mut Vec<Action>,
    ) {
        let children: Vec<_> = node
            .children
            .iter()
            .filter(|node| !node.tag.ends_with("Definitions"))
            .collect();
        let definitions = node
            .children
            .iter()
            .find(|node| node.tag == "Grid.ColumnDefinitions");
        let count = definitions
            .map_or(1, |node| node.children.len())
            .max(
                children
                    .iter()
                    .filter_map(|node| {
                        node.attr("Grid.Column")
                            .parse::<usize>()
                            .ok()
                            .map(|v| v + 1)
                    })
                    .max()
                    .unwrap_or(1),
            )
            .clamp(1, 16);
        let mut specs = vec!["*"; count];
        if let Some(definitions) = definitions {
            for (index, column) in definitions.children.iter().take(count).enumerate() {
                specs[index] = column.attr("Width");
            }
        }
        let mut auto = vec![0.0_f32; count];
        for child in &children {
            let column = child
                .attr("Grid.Column")
                .parse::<usize>()
                .unwrap_or(0)
                .min(count - 1);
            if specs[column].eq_ignore_ascii_case("Auto") {
                let text = replace(
                    if child.attr("Text").is_empty() {
                        child.attr("Title")
                    } else {
                        child.attr("Text")
                    },
                    values,
                );
                let text_width = ui
                    .painter()
                    .layout_no_wrap(
                        text,
                        egui::FontId::proportional(number(child.attr("FontSize")).unwrap_or(13.0)),
                        theme::palette(ui.ctx()).text,
                    )
                    .size()
                    .x;
                let margin = edges(child.attr("Margin"));
                auto[column] = auto[column].max(
                    number(child.attr("Width"))
                        .or_else(|| number(child.attr("MinWidth")))
                        .unwrap_or(
                            text_width
                                + if child.tag.contains("Button") {
                                    26.0
                                } else {
                                    0.0
                                },
                        )
                        + margin[0]
                        + margin[2],
                );
            }
        }
        let widths = grid_widths(&specs, &auto, ui.available_width());
        let row_definitions = node
            .children
            .iter()
            .find(|node| node.tag == "Grid.RowDefinitions");
        let rows = row_definitions
            .map_or(1, |node| node.children.len())
            .max(
                children
                    .iter()
                    .filter_map(|node| node.attr("Grid.Row").parse::<usize>().ok().map(|v| v + 1))
                    .max()
                    .unwrap_or(1),
            )
            .min(100);
        for row in 0..rows {
            let origin_pos = ui.next_widget_position();
            let min_height = row_definitions
                .and_then(|defs| defs.children.get(row))
                .and_then(|row| number(row.attr("Height")))
                .unwrap_or(0.0);
            let mut height = min_height;
            for column in 0..count {
                let nodes: Vec<_> = children
                    .iter()
                    .enumerate()
                    .filter(|(_, node)| {
                        node.attr("Grid.Row").parse::<usize>().unwrap_or(0) == row
                            && node.attr("Grid.Column").parse::<usize>().unwrap_or(0) == column
                    })
                    .collect();
                if nodes.is_empty() {
                    continue;
                }
                let left: f32 = widths[..column].iter().sum();
                let mut cell_y = origin_pos.y;
                for (index, node) in nodes {
                    let span = node
                        .attr("Grid.ColumnSpan")
                        .parse::<usize>()
                        .unwrap_or(1)
                        .clamp(1, count - column);
                    let width = widths[column..column + span].iter().sum::<f32>();
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(origin_pos.x + left, cell_y),
                        Vec2::new(width, ui.available_height().max(1000.0)),
                    );
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt(("xaml-grid", row, column, index))
                            .max_rect(rect),
                    );
                    child.spacing_mut().item_spacing.y = 0.0;
                    self.node(&mut child, node, origin, values, actions);
                    cell_y = child.min_rect().bottom();
                }
                height = height.max(cell_y - origin_pos.y);
            }
            ui.allocate_exact_size(Vec2::new(widths.iter().sum(), height), egui::Sense::hover());
        }
    }
    fn image(&mut self, ui: &mut egui::Ui, source: &str, node: &Node, origin: &Origin) {
        let resolved = resolve_image(source, origin);
        let key = match resolved {
            Ok(key) => key,
            Err(error) => {
                ui.colored_label(Color32::DARK_RED, format!("插图：{error:#}"));
                return;
            }
        };
        if let Some(Picture::Pending(receiver)) = self.images.get(&key) {
            match receiver.try_recv() {
                Ok(result) => {
                    let picture = match result {
                        Ok(image) => {
                            self.reserve_pixels(image.pixels.len());
                            Picture::Ready(ui.ctx().load_texture(
                                &key,
                                image,
                                egui::TextureOptions::LINEAR,
                            ))
                        }
                        Err(error) => Picture::Error(format!("{error:#}")),
                    };
                    self.images.insert(key.clone(), picture);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.images
                        .insert(key.clone(), Picture::Error("图片读取任务提前结束".into()));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if !self.images.contains_key(&key)
            && self
                .images
                .values()
                .filter(|image| matches!(image, Picture::Pending(_)))
                .count()
                < 4
        {
            if self.images.len() >= 24 {
                let old = self
                    .images
                    .iter()
                    .find(|(_, image)| !matches!(image, Picture::Pending(_)))
                    .map(|(key, _)| key.clone());
                if let Some(old) = old {
                    self.images.remove(&old);
                }
            }
            let (sender, receiver) = mpsc::channel();
            let request = key.clone();
            let repaint = ui.ctx().clone();
            match std::thread::Builder::new()
                .name("pcl-page-image".into())
                .spawn(move || {
                    let result = read_picture(&request);
                    let _ = sender.send(result);
                    repaint.request_repaint();
                }) {
                Ok(_) => {
                    self.images.insert(key.clone(), Picture::Pending(receiver));
                }
                Err(error) => {
                    self.images
                        .insert(key.clone(), Picture::Error(error.to_string()));
                }
            }
        }
        match self.images.get(&key) {
            Some(Picture::Ready(texture)) => {
                let native = texture.size_vec2();
                let width = number(node.attr("Width"))
                    .unwrap_or(native.x)
                    .min(ui.available_width())
                    .max(1.0);
                let height = number(node.attr("Height")).unwrap_or(width * native.y / native.x);
                ui.add(egui::Image::new(texture).fit_to_exact_size(Vec2::new(width, height)));
            }
            Some(Picture::Error(error)) => {
                let error = error.clone();
                ui.colored_label(Color32::DARK_RED, format!("图片加载失败：{error}"));
                if ui.button("重试图片").clicked() {
                    self.images.remove(&key);
                }
            }
            _ => {
                ui.label("正在加载图片……");
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
        }
    }
    fn reserve_pixels(&mut self, additional: usize) {
        loop {
            let used = self
                .images
                .values()
                .filter_map(|image| {
                    if let Picture::Ready(texture) = image {
                        Some(texture.size()[0] * texture.size()[1])
                    } else {
                        None
                    }
                })
                .sum::<usize>();
            if used + additional <= 16 * 1024 * 1024 {
                break;
            }
            let old = self
                .images
                .iter()
                .find_map(|(key, image)| matches!(image, Picture::Ready(_)).then(|| key.clone()));
            if let Some(old) = old {
                self.images.remove(&old);
            } else {
                break;
            }
        }
    }
    fn svg(&mut self, ui: &mut egui::Ui, source: &str, size: Vec2, color: Color32) {
        let key = format!("vector:{source}");
        if !self.images.contains_key(&key) {
            let result = (|| {
                let tree = resvg::usvg::Tree::from_str(source, &resvg::usvg::Options::default())?;
                let mut pixmap = resvg::tiny_skia::Pixmap::new(96, 96).context("矢量图尺寸错误")?;
                resvg::render(
                    &tree,
                    resvg::tiny_skia::Transform::from_scale(2.0, 2.0),
                    &mut pixmap.as_mut(),
                );
                anyhow::Ok(egui::ColorImage::from_rgba_premultiplied(
                    [96, 96],
                    pixmap.data(),
                ))
            })();
            self.images.insert(
                key.clone(),
                match result {
                    Ok(image) => Picture::Ready(ui.ctx().load_texture(
                        &key,
                        image,
                        egui::TextureOptions::LINEAR,
                    )),
                    Err(error) => Picture::Error(error.to_string()),
                },
            );
        }
        if let Some(Picture::Ready(texture)) = self.images.get(&key) {
            ui.add(
                egui::Image::new(texture)
                    .fit_to_exact_size(size)
                    .tint(color),
            );
        }
    }
}
fn public_markers(values: &HashMap<String, String>) -> HashMap<String, String> {
    values
        .iter()
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                "date" | "time" | "pcl_version" | "pcl_version_branch" | "pcl_build_type"
            )
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn grid_widths(specs: &[&str], auto: &[f32], available: f32) -> Vec<f32> {
    let mut widths = vec![0.0; specs.len()];
    let mut stars = vec![0.0; specs.len()];
    for (index, spec) in specs.iter().enumerate() {
        let spec = spec.trim();
        if spec.eq_ignore_ascii_case("Auto") {
            widths[index] = auto[index];
        } else if let Some(star) = spec.strip_suffix('*') {
            stars[index] = if star.is_empty() {
                1.0
            } else {
                number(star).unwrap_or(1.0)
            };
        } else if let Some(width) = number(spec) {
            widths[index] = width;
        } else {
            stars[index] = 1.0;
        }
    }
    let fixed = widths.iter().sum::<f32>();
    let total = stars.iter().sum::<f32>();
    let remaining = (available - fixed).max(0.0);
    for (index, star) in stars.into_iter().enumerate() {
        if total > 0.0 {
            widths[index] += remaining * star / total;
        }
    }
    widths
}

fn inline_actions(node: &Node) -> bool {
    node.children.iter().any(|child| {
        matches!(
            child.tag.as_str(),
            "MyTextButton" | "MyButton" | "MyIconTextButton" | "Hyperlink"
        ) || inline_actions(child)
    })
}

fn text_job(
    node: &Node,
    inherited: &egui::TextFormat,
    values: &HashMap<String, String>,
    job: &mut egui::text::LayoutJob,
) {
    let mut format = inherited.clone();
    if let Some(size) = number(node.attr("FontSize")) {
        format.font_id.size = size.clamp(6.0, 96.0);
    }
    if matches!(node.tag.as_str(), "Bold") || node.attr("FontWeight") == "Bold" {
        format.font_id.family = egui::FontFamily::Name("PCL Bold".into());
    }
    if node.tag == "Italic" {
        format.italics = true;
    }
    if matches!(node.tag.as_str(), "Underline" | "Hyperlink") {
        format.underline = egui::Stroke::new(1.0_f32, format.color);
    }
    if node.tag == "LineBreak" {
        job.append("\n", 0.0, format);
        return;
    }
    job.append(&replace(node.attr("Text"), values), 0.0, format.clone());
    for child in &node.children {
        text_job(child, &format, values, job);
    }
}
fn collect_actions(node: &Node, values: &HashMap<String, String>, actions: &mut Vec<Action>) {
    if !node.attr("EventType").is_empty() {
        actions.push(Action {
            kind: replace(node.attr("EventType"), values),
            data: replace(node.attr("EventData"), values),
        });
    }
    fn recurse(node: &Node, values: &HashMap<String, String>, actions: &mut Vec<Action>) {
        if node.tag == "CustomEvent" {
            actions.push(Action {
                kind: replace(node.attr("Type"), values),
                data: replace(node.attr("Data"), values),
            });
        }
        for child in &node.children {
            recurse(child, values, actions);
        }
    }
    for child in node
        .children
        .iter()
        .filter(|node| node.tag == "CustomEventService.Events")
    {
        recurse(child, values, actions);
    }
}
fn number(value: &str) -> Option<f32> {
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0 && *value <= 10_000.0)
}
fn edges(value: &str) -> [f32; 4] {
    let parts: Vec<_> = value
        .split(',')
        .filter_map(|part| part.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .collect();
    match parts.as_slice() {
        [all] => [*all; 4],
        [x, y] => [*x, *y, *x, *y],
        [l, t, r, b] => [*l, *t, *r, *b],
        _ => [0.0; 4],
    }
}
fn brush(value: &str, ctx: &egui::Context) -> Option<Color32> {
    let palette = theme::palette(ctx);
    if value.contains("ColorBrush1") {
        return Some(palette.text);
    }
    if value.contains("ColorBrush2") {
        return Some(palette.dark);
    }
    if value.contains("ColorBrush3") {
        return Some(palette.accent);
    }
    if value.contains("ColorBrush7") {
        return Some(palette.light);
    }
    if value.contains("ColorBrush4") {
        return Some(palette.border);
    }
    if value.contains("ColorBrush5") {
        return Some(palette.hover);
    }

    match value.to_ascii_lowercase().as_str() {
        "white" => Some(Color32::WHITE),
        "black" => Some(Color32::BLACK),
        "transparent" => Some(Color32::TRANSPARENT),
        "red" => Some(Color32::RED),
        _ => {
            let hex = value.strip_prefix('#')?;
            let int = u32::from_str_radix(hex, 16).ok()?;
            match hex.len() {
                6 => Some(Color32::from_rgb(
                    (int >> 16) as u8,
                    (int >> 8) as u8,
                    int as u8,
                )),
                8 => Some(Color32::from_rgba_unmultiplied(
                    (int >> 16) as u8,
                    (int >> 8) as u8,
                    int as u8,
                    (int >> 24) as u8,
                )),
                _ => None,
            }
        }
    }
}
pub(super) fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn resolve_image(source: &str, origin: &Origin) -> Result<String> {
    if (origin.url.is_some() || source.starts_with("http"))
        && (source.contains('{') || source.contains('}'))
    {
        bail!("网络插图包含未知或非公开替换标记，未发送请求");
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        return Ok(http_url(source)?.to_string());
    }
    if let Some(url) = &origin.url {
        return Ok(http_url(url)?
            .join(source)
            .and_then(|url| reqwest::Url::parse(url.as_str()))
            .context("相对图片网址无效")
            .and_then(|url| http_url(url.as_str()))?
            .to_string());
    }
    let root = origin
        .directory
        .as_deref()
        .context("插图没有本地来源目录")?
        .canonicalize()?;
    let relative = pcl_core::metadata::safe_relative(source)?;
    let path = root
        .join(relative)
        .canonicalize()
        .context("本地插图不存在")?;
    if !path.starts_with(&root) || !path.is_file() {
        bail!("本地插图超出主页文件夹");
    }
    Ok(path.to_string_lossy().into_owned())
}
fn read_picture(source: &str) -> Result<egui::ColorImage> {
    let bytes = if source.starts_with("http://") || source.starts_with("https://") {
        fetch_bytes(source, 8 * 1024 * 1024)?
    } else {
        let file = std::fs::File::open(Path::new(source))?;
        let mut bytes = Vec::new();
        file.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 8 * 1024 * 1024 {
            bail!("插图超过 8 MiB");
        }
        bytes
    };
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(20 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?.to_rgba8();
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        &image,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dock_panel_reserves_edges_and_fills_remaining_rectangle() {
        let nodes=parse(r##"<DockPanel Width="300" Height="100"><Border Width="80" DockPanel.Dock="Left" Background="#FF0000"/><Border Height="20" DockPanel.Dock="Top" Background="#0000FF"/><Border Background="#00FF00"/></DockPanel>"##).unwrap();
        let mut renderer = Renderer::default();
        let ctx = egui::Context::default();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                renderer.render(ui, &nodes, &Origin::default(), &HashMap::new());
            });
        });
        let rectangle = |color| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if rect.fill == color => Some(rect.rect),
                    _ => None,
                })
                .expect("visible dock rectangle")
        };
        let left = rectangle(Color32::RED);
        let top = rectangle(Color32::BLUE);
        let fill = rectangle(Color32::GREEN);
        assert_eq!(left.size(), Vec2::new(80.0, 100.0));
        assert_eq!(top.size(), Vec2::new(220.0, 20.0));
        assert_eq!(fill.size(), Vec2::new(220.0, 80.0));
        assert_eq!(left.right(), top.left());
        assert_eq!(top.bottom(), fill.top());
        assert_eq!(left.bottom(), fill.bottom());
    }
    #[test]
    fn binding_reads_live_named_controls_and_visual_triggers_do_not_emit_actions() {
        let nodes=parse(r#"<StackPanel><local:MyTextBox Name="entry" Text="first"/><TextBlock Text="{Binding Text, ElementName=entry}"><TextBlock.Triggers><DataTrigger Binding="{Binding Text, ElementName=entry}" Value="second"><Setter Property="Foreground" Value="Red"/></DataTrigger></TextBlock.Triggers></TextBlock></StackPanel>"#).unwrap();
        let mut renderer = Renderer::default();
        let values = HashMap::new();
        renderer.seed_named_inputs(&nodes, &values);
        let target = &nodes[0].children[1];
        assert_eq!(
            renderer
                .resolved_node(target, &values, false, false)
                .attr("Text"),
            "first"
        );
        renderer.inputs.insert("entry".into(), "second".into());
        let resolved = renderer.resolved_node(target, &values, false, false);
        assert_eq!(resolved.attr("Text"), "second");
        assert_eq!(resolved.attr("Foreground"), "Red");
        let ctx = egui::Context::default();
        let mut actions = Vec::new();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                actions = renderer.render(ui, &nodes, &Origin::default(), &values)
            });
        });
        assert!(actions.is_empty());
    }
    #[test]
    fn style_hover_trigger_is_visual_only_and_binding_cannot_exfiltrate() {
        let nodes=parse(r#"<StackPanel><StackPanel.Resources><Style x:Key="Hover" TargetType="TextBlock"><Style.Triggers><Trigger Property="IsMouseOver" Value="True"><Setter Property="Foreground" Value="Red"/></Trigger></Style.Triggers></Style></StackPanel.Resources><TextBlock Style="{StaticResource Hover}" Text="hover"/></StackPanel>"#).unwrap();
        let renderer = Renderer::default();
        let target = &nodes[0].children[0];
        assert_eq!(
            renderer
                .resolved_node(target, &HashMap::new(), true, false)
                .attr("Foreground"),
            "Red"
        );
        assert_eq!(
            renderer
                .resolved_node(target, &HashMap::new(), false, false)
                .attr("Foreground"),
            ""
        );
        for text in [
            r#"<Image Source="{Binding secret}"/>"#,
            r#"<TextBlock Text="{Binding secret, Converter=Execute}"/>"#,
            r#"<TextBlock><TextBlock.Triggers><Trigger Property="IsMouseOver" Value="True"><Setter Property="EventType" Value="执行命令"/></Trigger></TextBlock.Triggers></TextBlock>"#,
            r#"<StackPanel><StackPanel.Resources><Style TargetType="Image"><Setter Property="Source" Value="{Binding secret}"/></Style></StackPanel.Resources><Image/></StackPanel>"#,
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
    }
    #[test]
    fn static_styles_strings_and_templates_materialize_without_automatic_events() {
        let nodes=parse(r#"<StackPanel xmlns:sys="clr-namespace:System;assembly=mscorlib"><StackPanel.Triggers/><StackPanel.Resources><Style TargetType="TextBlock"><Setter Property="FontSize" Value="14"/></Style><Style x:Key="Action" TargetType="local:MyButton"><Setter Property="Text" Value="Visit"/><Setter Property="EventType" Value="打开网页"/></Style><sys:String x:Key="Icon">M0 0L1 1</sys:String><ControlTemplate x:Key="Sep"><TextBlock Text="{TemplateBinding Content}"/></ControlTemplate></StackPanel.Resources><local:MyButton Style="{StaticResource Action}" EventData="https://example.org" Logo="{StaticResource Icon}"/><ContentControl Template="{StaticResource Sep}" Content="Heading"/><FlowDocument FontSize="15"><Paragraph>Actual body</Paragraph></FlowDocument></StackPanel>"#).unwrap();
        let root = &nodes[0];
        assert_eq!(root.children.len(), 3);
        assert_eq!(root.children[0].attr("Text"), "Visit");
        assert_eq!(root.children[0].attr("EventType"), "打开网页");
        assert_eq!(root.children[0].attr("Logo"), "M0 0L1 1");
        assert_eq!(root.children[1].children[0].attr("Text"), "Heading");
        assert_eq!(root.children[1].children[0].attr("FontSize"), "14");
        assert_eq!(root.children[2].children[0].attr("FontSize"), "15");
        assert!(parse(
            "<StackPanel><StackPanel.Triggers><EventTrigger/></StackPanel.Triggers></StackPanel>"
        )
        .is_err());
        assert!(parse("<StackPanel><StackPanel.Resources><Style TargetType='Grid'><Setter Property='Loaded' Value='Run'/></Style></StackPanel.Resources><Grid/></StackPanel>").is_err());
    }
    #[test]
    fn automatic_image_markers_exclude_local_paths_accounts_and_variables() {
        let values = HashMap::from([
            ("date".into(), "2026/10/5".into()),
            ("minecraft".into(), "/private/game".into()),
            ("variable:secret".into(), "not-public".into()),
        ]);
        assert_eq!(public_markers(&values).len(), 1);
        let source = replace(
            "https://example.org/image?path={minecraft}",
            &public_markers(&values),
        );
        assert!(resolve_image(&source, &Origin::default()).is_err());
    }
    #[test]
    fn grid_preserves_fixed_auto_and_weighted_star_columns() {
        assert_eq!(
            grid_widths(&["100", "Auto", "2*", "*"], &[0.0, 40.0, 0.0, 0.0], 440.0),
            vec![100.0, 40.0, 200.0, 100.0]
        );
    }
    #[test]
    fn parses_layout_and_events_as_data_rejects_active_objects() {
        let nodes=parse("<local:MyCard Title='示例'><StackPanel><TextBlock>Hello <Bold>world</Bold></TextBlock><local:MyButton Text='复制' EventType='复制文本' EventData='hello'/></StackPanel></local:MyCard>").unwrap();
        assert_eq!(nodes[0].children[0].children.len(), 2);
        assert!(parse("<ObjectDataProvider/>").is_err());
        assert!(parse("<Grid Loaded='evil'/>").is_err());
        assert!(parse("<!DOCTYPE x [<!ENTITY a SYSTEM 'file:///etc/passwd'>]><Grid/>").is_err());
    }
    #[test]
    fn markers_preserve_unknown_and_do_not_reparse_injected_markup() {
        let values = HashMap::from([
            ("name".into(), "<Button> & 中文".into()),
            ("variable:x".into(), "actual".into()),
        ]);
        assert_eq!(
            replace(
                "{name}/{variable:x:d}/{varible:y:default}/{unknown}",
                &values
            ),
            "<Button> & 中文/actual/default/{unknown}"
        );
    }
    #[test]
    fn network_origin_cannot_read_local_files_or_credentialed_urls() {
        let origin = Origin {
            url: Some("https://example.org/page/Custom.xaml".into()),
            directory: None,
        };
        assert_eq!(
            resolve_image("../a.png", &origin).unwrap(),
            "https://example.org/a.png"
        );
        assert!(resolve_image("file:///etc/passwd", &origin).is_err());
        assert!(http_url("https://user:secret@example.org/").is_err());
    }
    #[test]
    fn local_images_confined_even_through_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("a.png"), b"x").unwrap();
        let origin = Origin {
            directory: Some(dir.path().to_owned()),
            url: None,
        };
        assert!(resolve_image("../a.png", &origin).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path().join("a.png"), dir.path().join("a.png"))
                .unwrap();
            assert!(resolve_image("a.png", &origin).is_err());
        }
    }
    #[test]
    fn rendering_does_not_execute_or_emit_events_without_input() {
        let ctx = egui::Context::default();
        let nodes =
            parse("<local:MyButton Text='command' EventType='执行命令' EventData='dangerous'/>")
                .unwrap();
        let mut renderer = Renderer::default();
        let mut events = Vec::new();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                events = renderer.render(ui, &nodes, &Origin::default(), &HashMap::new());
            });
        });
        assert!(events.is_empty());
    }
}
