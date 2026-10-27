//! CMSIS-SVD (System View Description) parser.
//!
//! Supports the parts of the format used by STM32 (and most other) SVD files:
//! `derivedFrom` on peripherals/clusters/registers/fields, `dim` arrays,
//! clusters, inherited register properties, `bitOffset`/`bitWidth`,
//! `lsb`/`msb` and `bitRange` field positions, enumerated values and
//! `readAction` (registers whose reads have side effects are never read
//! automatically).

use crate::util::{hex, mask};
use anyhow::{anyhow, bail, Context, Result};
use roxmltree::{Document, Node};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Access {
    #[default]
    ReadWrite,
    ReadOnly,
    WriteOnly,
    WriteOnce,
    ReadWriteOnce,
}

impl Access {
    fn parse(text: &str) -> Option<Self> {
        Some(match text.trim() {
            "read-write" => Access::ReadWrite,
            "read-only" => Access::ReadOnly,
            "write-only" => Access::WriteOnly,
            "writeOnce" => Access::WriteOnce,
            "read-writeOnce" => Access::ReadWriteOnce,
            _ => return None,
        })
    }

    pub fn readable(self) -> bool {
        !matches!(self, Access::WriteOnly | Access::WriteOnce)
    }

    pub fn writable(self) -> bool {
        !matches!(self, Access::ReadOnly)
    }

    pub fn short(self) -> &'static str {
        match self {
            Access::ReadWrite => "rw",
            Access::ReadOnly => "ro",
            Access::WriteOnly => "wo",
            Access::WriteOnce => "w1",
            Access::ReadWriteOnce => "rw1",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumValue {
    pub name: String,
    pub description: String,
    pub value: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub description: String,
    pub lsb: u32,
    pub width: u32,
    pub access: Access,
    pub enums: Vec<EnumValue>,
}

impl Field {
    pub fn msb(&self) -> u32 {
        self.lsb + self.width - 1
    }

    pub fn extract(&self, register_value: u64) -> u64 {
        (register_value >> self.lsb) & mask(self.width)
    }

    pub fn insert(&self, register_value: u64, field_value: u64) -> u64 {
        let m = mask(self.width) << self.lsb;
        (register_value & !m) | ((field_value << self.lsb) & m)
    }

    pub fn bit_range(&self) -> String {
        if self.width == 1 {
            format!("[{}]", self.lsb)
        } else {
            format!("[{}:{}]", self.msb(), self.lsb)
        }
    }

    pub fn enum_name(&self, value: u64) -> Option<&EnumValue> {
        self.enums.iter().find(|e| e.value == value)
    }

    /// Human readable value: `1`, `5`, `0x1f`, or `Output (1)` for enumerations.
    pub fn format(&self, value: u64) -> String {
        let number = if self.width <= 4 {
            value.to_string()
        } else {
            hex(value, self.width)
        };
        match self.enum_name(value) {
            Some(e) => format!("{} ({number})", e.name),
            None => number,
        }
    }

    /// Parses user input: a number or the name of an enumerated value.
    pub fn parse_input(&self, text: &str) -> Result<u64> {
        let t = text.trim();
        let candidate = t.split_whitespace().next().unwrap_or(t);
        if let Some(e) = self
            .enums
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(candidate))
        {
            return Ok(e.value);
        }
        let value = match t.to_ascii_lowercase().as_str() {
            "true" | "on" | "set" => 1,
            "false" | "off" | "clear" => 0,
            _ => crate::util::parse_u64(candidate).ok_or_else(|| {
                anyhow!("`{t}` is not a number or a known value of {}", self.name)
            })?,
        };
        if value > mask(self.width) {
            bail!(
                "{value} does not fit into {} ({} bit{})",
                self.name,
                self.width,
                if self.width == 1 { "" } else { "s" }
            );
        }
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Register {
    pub name: String,
    pub description: String,
    /// Absolute address.
    pub address: u64,
    /// Width in bits.
    pub size: u32,
    pub access: Access,
    pub reset_value: u64,
    /// `true` when reading the register has side effects (`readAction`).
    pub read_side_effects: bool,
    pub fields: Vec<Field>,
}

impl Register {
    pub fn bytes(&self) -> u64 {
        u64::from(self.size.div_ceil(8))
    }

    /// Whether the debugger may read this register without being asked to.
    pub fn auto_readable(&self) -> bool {
        self.access.readable() && !self.read_side_effects
    }

    pub fn format(&self, value: u64) -> String {
        hex(value, self.size)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    pub name: String,
    pub description: String,
    pub address: u64,
    pub children: Vec<RegisterNode>,
}

/// A child of a peripheral or cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterNode {
    Register(Register),
    Cluster(Cluster),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peripheral {
    pub name: String,
    pub description: String,
    pub group: Option<String>,
    pub base_address: u64,
    pub children: Vec<RegisterNode>,
}

impl Peripheral {
    /// All registers, depth first.
    pub fn registers(&self) -> Vec<&Register> {
        fn walk<'a>(nodes: &'a [RegisterNode], out: &mut Vec<&'a Register>) {
            for n in nodes {
                match n {
                    RegisterNode::Register(r) => out.push(r),
                    RegisterNode::Cluster(c) => walk(&c.children, out),
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.children, &mut out);
        out
    }

    pub fn find_register(&self, path: &str) -> Option<&Register> {
        self.registers()
            .into_iter()
            .find(|r| r.name.eq_ignore_ascii_case(path))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub description: String,
    pub peripherals: Vec<Peripheral>,
}

impl Device {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read SVD file {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid SVD file {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let doc = Document::parse(text)?;
        let root = doc.root_element();
        if root.tag_name().name() != "device" {
            bail!(
                "root element is <{}>, expected <device>",
                root.tag_name().name()
            );
        }
        let defaults = Props::default().merge(root);
        let peripherals_node =
            child(root, "peripherals").ok_or_else(|| anyhow!("no <peripherals>"))?;
        let all: Vec<Node> = elements(peripherals_node, "peripheral").collect();
        let by_name: HashMap<String, Node> = all
            .iter()
            .filter_map(|p| Some((text_of(*p, "name")?, *p)))
            .collect();

        let mut peripherals = Vec::new();
        for p in &all {
            let base = match p.attribute("derivedFrom") {
                Some(name) => Some(*by_name.get(name.trim()).ok_or_else(|| {
                    anyhow!("peripheral derivedFrom unknown peripheral `{name}`")
                })?),
                None => None,
            };
            let lookup =
                |tag: &str| text_of(*p, tag).or_else(|| base.and_then(|b| text_of(b, tag)));
            let name = lookup("name").ok_or_else(|| anyhow!("peripheral without <name>"))?;
            let base_address = lookup("baseAddress")
                .and_then(|t| parse_scaled(&t))
                .ok_or_else(|| anyhow!("peripheral {name} has no valid <baseAddress>"))?;
            let description = lookup("description").map(clean).unwrap_or_default();
            let group = lookup("groupName");
            let mut props = defaults;
            if let Some(b) = base {
                props = props.merge(b);
            }
            props = props.merge(*p);
            let registers_node =
                child(*p, "registers").or_else(|| base.and_then(|b| child(b, "registers")));
            for (index, offset) in dim_instances(*p, &name)? {
                let children = match registers_node {
                    Some(regs) => parse_children(regs, base_address + offset, props)?,
                    None => Vec::new(),
                };
                peripherals.push(Peripheral {
                    name: index,
                    description: description.clone(),
                    group: group.clone(),
                    base_address: base_address + offset,
                    children,
                });
            }
        }
        peripherals.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Device {
            name: text_of(root, "name").unwrap_or_else(|| "device".into()),
            description: text_of(root, "description").map(clean).unwrap_or_default(),
            peripherals,
        })
    }

    pub fn find_peripheral(&self, name: &str) -> Option<&Peripheral> {
        self.peripherals
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }
}

/// Register properties inherited down the device → peripheral → cluster → register chain.
#[derive(Debug, Clone, Copy)]
struct Props {
    size: u32,
    access: Access,
    reset_value: u64,
}

impl Default for Props {
    fn default() -> Self {
        Self {
            size: 32,
            access: Access::ReadWrite,
            reset_value: 0,
        }
    }
}

impl Props {
    fn merge(mut self, node: Node) -> Self {
        if let Some(size) = text_of(node, "size").and_then(|t| parse_scaled(&t)) {
            self.size = size as u32;
        }
        if let Some(access) = text_of(node, "access").and_then(|t| Access::parse(&t)) {
            self.access = access;
        }
        if let Some(reset) = text_of(node, "resetValue").and_then(|t| parse_scaled(&t)) {
            self.reset_value = reset;
        }
        self
    }
}

fn parse_children(parent: Node, base_address: u64, props: Props) -> Result<Vec<RegisterNode>> {
    let siblings: HashMap<String, Node> = parent
        .children()
        .filter(|n| n.is_element() && matches!(n.tag_name().name(), "register" | "cluster"))
        .filter_map(|n| Some((text_of(n, "name")?, n)))
        .collect();
    let mut out = Vec::new();
    for node in parent.children().filter(Node::is_element) {
        let kind = node.tag_name().name();
        if kind != "register" && kind != "cluster" {
            continue;
        }
        let base = node.attribute("derivedFrom").and_then(|name| {
            let short = name.rsplit('.').next().unwrap_or(name).trim();
            siblings.get(short).copied()
        });
        let lookup = |tag: &str| text_of(node, tag).or_else(|| base.and_then(|b| text_of(b, tag)));
        let name = lookup("name").ok_or_else(|| anyhow!("<{kind}> without <name>"))?;
        let offset = lookup("addressOffset")
            .and_then(|t| parse_scaled(&t))
            .ok_or_else(|| anyhow!("{kind} {name} has no valid <addressOffset>"))?;
        let description = lookup("description").map(clean).unwrap_or_default();
        let mut own_props = props;
        if let Some(b) = base {
            own_props = own_props.merge(b);
        }
        own_props = own_props.merge(node);

        for (instance_name, dim_offset) in dim_instances(node, &name)? {
            let address = base_address + offset + dim_offset;
            if kind == "cluster" {
                // Children may come from the derived node or its base.
                let has_own = node.children().any(|c| {
                    c.is_element() && matches!(c.tag_name().name(), "register" | "cluster")
                });
                let source = if has_own { node } else { base.unwrap_or(node) };
                out.push(RegisterNode::Cluster(Cluster {
                    name: instance_name,
                    description: description.clone(),
                    address,
                    children: parse_children(source, address, own_props)?,
                }));
            } else {
                let fields_node =
                    child(node, "fields").or_else(|| base.and_then(|b| child(b, "fields")));
                let mut fields = match fields_node {
                    Some(f) => parse_fields(f, own_props.access)?,
                    None => Vec::new(),
                };
                fields.sort_by(|a, b| b.lsb.cmp(&a.lsb));
                out.push(RegisterNode::Register(Register {
                    name: instance_name,
                    description: description.clone(),
                    address,
                    size: own_props.size,
                    access: own_props.access,
                    reset_value: own_props.reset_value,
                    read_side_effects: lookup("readAction").is_some(),
                    fields,
                }));
            }
        }
    }
    out.sort_by_key(|n| match n {
        RegisterNode::Register(r) => r.address,
        RegisterNode::Cluster(c) => c.address,
    });
    Ok(out)
}

fn parse_fields(fields_node: Node, register_access: Access) -> Result<Vec<Field>> {
    let siblings: HashMap<String, Node> = elements(fields_node, "field")
        .filter_map(|n| Some((text_of(n, "name")?, n)))
        .collect();
    let mut out = Vec::new();
    for node in elements(fields_node, "field") {
        let base = node.attribute("derivedFrom").and_then(|name| {
            siblings
                .get(name.rsplit('.').next().unwrap_or(name).trim())
                .copied()
        });
        let lookup = |tag: &str| text_of(node, tag).or_else(|| base.and_then(|b| text_of(b, tag)));
        let name = lookup("name").ok_or_else(|| anyhow!("<field> without <name>"))?;
        let (lsb, width) = if let (Some(o), Some(w)) = (lookup("bitOffset"), lookup("bitWidth")) {
            (
                parse_scaled(&o).ok_or_else(|| anyhow!("bad bitOffset in {name}"))? as u32,
                parse_scaled(&w).ok_or_else(|| anyhow!("bad bitWidth in {name}"))? as u32,
            )
        } else if let (Some(l), Some(m)) = (lookup("lsb"), lookup("msb")) {
            let l = parse_scaled(&l).ok_or_else(|| anyhow!("bad lsb in {name}"))? as u32;
            let m = parse_scaled(&m).ok_or_else(|| anyhow!("bad msb in {name}"))? as u32;
            (l.min(m), l.abs_diff(m) + 1)
        } else if let Some(range) = lookup("bitRange") {
            parse_bit_range(&range).ok_or_else(|| anyhow!("bad bitRange `{range}` in {name}"))?
        } else {
            bail!("field {name} has no bit position");
        };
        if width == 0 || width > 64 {
            bail!("field {name} has invalid width {width}");
        }
        let access = lookup("access")
            .and_then(|t| Access::parse(&t))
            .unwrap_or(register_access);
        let description = lookup("description").map(clean).unwrap_or_default();
        let enum_source = if node.children().any(|c| c.has_tag_name("enumeratedValues")) {
            Some(node)
        } else {
            base
        };
        let enums = enum_source
            .map(|n| parse_enums(n, width))
            .unwrap_or_default();
        let instances = dim_instances(node, &name)?;
        let dim_step = instances.len() > 1;
        for (instance_name, offset) in instances {
            let bit_offset = if dim_step { offset as u32 } else { 0 };
            out.push(Field {
                name: instance_name,
                description: description.clone(),
                lsb: lsb + bit_offset,
                width,
                access,
                enums: enums.clone(),
            });
        }
    }
    Ok(out)
}

/// Chooses the enumeratedValues block used for reading and expands it.
fn parse_enums(field: Node, width: u32) -> Vec<EnumValue> {
    let blocks: Vec<Node> = elements(field, "enumeratedValues").collect();
    let chosen = blocks
        .iter()
        .find(|b| {
            matches!(
                text_of(**b, "usage").as_deref(),
                None | Some("read") | Some("read-write")
            )
        })
        .or(blocks.first());
    let Some(block) = chosen else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ev in elements(*block, "enumeratedValue") {
        let Some(name) = text_of(ev, "name") else {
            continue;
        };
        let Some(value_text) = text_of(ev, "value") else {
            continue; // `isDefault` entries have no single value
        };
        let description = text_of(ev, "description").map(clean).unwrap_or_default();
        for value in expand_enum_value(&value_text, width) {
            out.push(EnumValue {
                name: name.clone(),
                description: description.clone(),
                value,
            });
        }
    }
    out
}

/// Expands values like `#1x0` (x = don't care) into concrete numbers.
fn expand_enum_value(text: &str, width: u32) -> Vec<u64> {
    let t = text.trim();
    if let Some(bits) = t.strip_prefix('#') {
        if bits.contains(['x', 'X']) {
            let mut values = vec![0u64];
            for c in bits.chars() {
                values = match c {
                    '0' => values.iter().map(|v| v << 1).collect(),
                    '1' => values.iter().map(|v| (v << 1) | 1).collect(),
                    'x' | 'X' => values.iter().flat_map(|v| [v << 1, (v << 1) | 1]).collect(),
                    _ => return Vec::new(),
                };
                if values.len() > 64 {
                    return Vec::new();
                }
            }
            return values.into_iter().filter(|v| *v <= mask(width)).collect();
        }
    }
    parse_scaled(t).into_iter().collect()
}

/// Expands `dim` arrays. Returns `(name, address_or_bit_offset_increment)` pairs.
fn dim_instances(node: Node, name: &str) -> Result<Vec<(String, u64)>> {
    let Some(dim) = text_of(node, "dim").and_then(|t| parse_scaled(&t)) else {
        return Ok(vec![(name.replace("%s", "").replace("[]", ""), 0)]);
    };
    let increment = text_of(node, "dimIncrement")
        .and_then(|t| parse_scaled(&t))
        .unwrap_or(0);
    let indices: Vec<String> = match text_of(node, "dimIndex") {
        Some(spec) => parse_dim_index(&spec).ok_or_else(|| anyhow!("bad dimIndex `{spec}`"))?,
        None => (0..dim).map(|i| i.to_string()).collect(),
    };
    if indices.len() as u64 != dim {
        bail!(
            "{name}: dimIndex has {} entries but dim is {dim}",
            indices.len()
        );
    }
    Ok(indices
        .iter()
        .enumerate()
        .map(|(i, idx)| {
            let n = if name.contains("[%s]") {
                name.replace("[%s]", &format!("[{idx}]"))
            } else if name.contains("%s") {
                name.replace("%s", idx)
            } else {
                format!("{name}{idx}")
            };
            (n, increment * i as u64)
        })
        .collect())
}

fn parse_dim_index(spec: &str) -> Option<Vec<String>> {
    let s = spec.trim();
    if let Some((a, b)) = s.split_once('-') {
        if let (Ok(a), Ok(b)) = (a.trim().parse::<u64>(), b.trim().parse::<u64>()) {
            return (a <= b).then(|| (a..=b).map(|i| i.to_string()).collect());
        }
        let (a, b) = (a.trim(), b.trim());
        if a.len() == 1 && b.len() == 1 {
            let (a, b) = (a.chars().next()?, b.chars().next()?);
            return (a <= b).then(|| (a..=b).map(|c| c.to_string()).collect());
        }
        return None;
    }
    Some(s.split(',').map(|p| p.trim().to_string()).collect())
}

fn parse_bit_range(text: &str) -> Option<(u32, u32)> {
    let inner = text.trim().strip_prefix('[')?.strip_suffix(']')?;
    let (m, l) = inner.split_once(':')?;
    let m: u32 = m.trim().parse().ok()?;
    let l: u32 = l.trim().parse().ok()?;
    Some((l.min(m), l.abs_diff(m) + 1))
}

/// Parses an SVD `scaledNonNegativeInteger`.
pub fn parse_scaled(text: &str) -> Option<u64> {
    let t = text.trim();
    let (number, multiplier) = match t.chars().last()? {
        'k' | 'K' if !t.starts_with("0x") && !t.starts_with("0X") => {
            (&t[..t.len() - 1], 1u64 << 10)
        }
        'm' | 'M' => (&t[..t.len() - 1], 1 << 20),
        'g' | 'G' => (&t[..t.len() - 1], 1 << 30),
        _ => (t, 1),
    };
    crate::util::parse_u64(number).map(|v| v * multiplier)
}

fn child<'a, 'i>(node: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == tag)
}

fn elements<'a, 'i: 'a>(
    node: Node<'a, 'i>,
    tag: &'a str,
) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    node.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == tag)
}

fn text_of(node: Node, tag: &str) -> Option<String> {
    child(node, tag)
        .and_then(|c| c.text())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Collapses the whitespace SVD descriptions are usually wrapped with.
fn clean(text: String) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Groups readable registers into contiguous memory ranges `(address, length)`
/// so that a peripheral can be read with a few memory transactions.
pub fn read_plan(registers: &[&Register]) -> Vec<(u64, u64)> {
    let mut spans: Vec<(u64, u64)> = registers
        .iter()
        .filter(|r| r.auto_readable())
        .map(|r| (r.address, r.address + r.bytes()))
        .collect();
    spans.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 && end - last.0 <= 1024 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged.into_iter().map(|(s, e)| (s, e - s)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<device schemaVersion="1.1">
  <name>STM32TEST</name>
  <description>Test device</description>
  <size>32</size>
  <access>read-write</access>
  <resetValue>0x00000000</resetValue>
  <peripherals>
    <peripheral>
      <name>GPIOA</name>
      <description>General-purpose I/Os</description>
      <groupName>GPIO</groupName>
      <baseAddress>0x40020000</baseAddress>
      <registers>
        <register>
          <name>MODER</name>
          <description>GPIO port mode
            register</description>
          <addressOffset>0x0</addressOffset>
          <resetValue>0xA8000000</resetValue>
          <fields>
            <field>
              <name>MODER%s</name>
              <description>Port x configuration bits</description>
              <bitOffset>0</bitOffset>
              <bitWidth>2</bitWidth>
              <dim>16</dim>
              <dimIncrement>2</dimIncrement>
              <enumeratedValues>
                <enumeratedValue><name>Input</name><value>0</value></enumeratedValue>
                <enumeratedValue><name>Output</name><value>1</value></enumeratedValue>
                <enumeratedValue><name>Alternate</name><value>2</value></enumeratedValue>
                <enumeratedValue><name>Analog</name><value>3</value></enumeratedValue>
              </enumeratedValues>
            </field>
          </fields>
        </register>
        <register>
          <name>IDR</name>
          <addressOffset>0x10</addressOffset>
          <access>read-only</access>
          <fields>
            <field><name>IDR0</name><bitRange>[0:0]</bitRange></field>
          </fields>
        </register>
        <register>
          <name>ODR</name>
          <addressOffset>0x14</addressOffset>
          <fields>
            <field><name>ODR15</name><lsb>15</lsb><msb>15</msb></field>
            <field><name>ODR0</name><bitOffset>0</bitOffset><bitWidth>1</bitWidth></field>
          </fields>
        </register>
        <register>
          <name>BSRR</name>
          <addressOffset>0x18</addressOffset>
          <access>write-only</access>
        </register>
      </registers>
    </peripheral>
    <peripheral derivedFrom="GPIOA">
      <name>GPIOB</name>
      <baseAddress>0x40020400</baseAddress>
    </peripheral>
    <peripheral>
      <name>USART1</name>
      <baseAddress>0x40011000</baseAddress>
      <registers>
        <register>
          <name>SR</name>
          <addressOffset>0x0</addressOffset>
          <size>0x20</size>
          <resetValue>0x00C0</resetValue>
          <fields>
            <field><name>TXE</name><bitOffset>7</bitOffset><bitWidth>1</bitWidth></field>
            <field><name>TC</name><bitOffset>6</bitOffset><bitWidth>1</bitWidth></field>
          </fields>
        </register>
        <register>
          <name>DR</name>
          <addressOffset>0x4</addressOffset>
          <readAction>clear</readAction>
          <fields>
            <field><name>DR</name><bitOffset>0</bitOffset><bitWidth>9</bitWidth></field>
          </fields>
        </register>
        <register derivedFrom="SR">
          <name>SR_COPY</name>
          <addressOffset>0x8</addressOffset>
        </register>
      </registers>
    </peripheral>
    <peripheral>
      <name>DMA1</name>
      <baseAddress>0x40026000</baseAddress>
      <registers>
        <cluster>
          <dim>2</dim>
          <dimIncrement>0x18</dimIncrement>
          <name>S[%s]</name>
          <addressOffset>0x10</addressOffset>
          <register>
            <name>CR</name>
            <addressOffset>0x0</addressOffset>
            <fields>
              <field>
                <name>DIR</name><bitOffset>6</bitOffset><bitWidth>2</bitWidth>
                <enumeratedValues>
                  <enumeratedValue><name>P2M</name><value>#00</value></enumeratedValue>
                  <enumeratedValue><name>M2P</name><value>#01</value></enumeratedValue>
                  <enumeratedValue><name>M2M_or_reserved</name><value>#1x</value></enumeratedValue>
                </enumeratedValues>
              </field>
            </fields>
          </register>
          <register>
            <name>NDTR</name>
            <addressOffset>0x4</addressOffset>
            <size>16</size>
          </register>
        </cluster>
        <register>
          <dim>3</dim>
          <dimIncrement>4</dimIncrement>
          <dimIndex>A,B,C</dimIndex>
          <name>FLAG%s</name>
          <addressOffset>0x100</addressOffset>
        </register>
      </registers>
    </peripheral>
  </peripherals>
</device>
"#;

    fn device() -> Device {
        Device::parse(SAMPLE).unwrap()
    }

    #[test]
    fn parses_peripherals_sorted_by_name() {
        let d = device();
        let names: Vec<_> = d.peripherals.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["DMA1", "GPIOA", "GPIOB", "USART1"]);
        assert_eq!(d.name, "STM32TEST");
    }

    #[test]
    fn expands_field_arrays_and_enums() {
        let d = device();
        let moder = d
            .find_peripheral("gpioa")
            .unwrap()
            .find_register("MODER")
            .unwrap();
        assert_eq!(moder.fields.len(), 16);
        assert_eq!(moder.description, "GPIO port mode register");
        let f15 = &moder.fields[0];
        assert_eq!(f15.name, "MODER15");
        assert_eq!((f15.lsb, f15.width), (30, 2));
        assert_eq!(f15.extract(0xA800_0000), 2);
        assert_eq!(f15.format(2), "Alternate (2)");
        assert_eq!(f15.bit_range(), "[31:30]");
        assert_eq!(moder.reset_value, 0xA800_0000);
    }

    #[test]
    fn supports_all_bit_position_styles() {
        let d = device();
        let gpio = d.find_peripheral("GPIOA").unwrap();
        let idr = gpio.find_register("IDR").unwrap();
        assert_eq!((idr.fields[0].lsb, idr.fields[0].width), (0, 1));
        assert_eq!(idr.access, Access::ReadOnly);
        assert_eq!(idr.fields[0].access, Access::ReadOnly);
        let odr = gpio.find_register("ODR").unwrap();
        assert_eq!(odr.fields[0].name, "ODR15");
        assert_eq!((odr.fields[0].lsb, odr.fields[0].width), (15, 1));
    }

    #[test]
    fn resolves_derived_peripherals() {
        let d = device();
        let b = d.find_peripheral("GPIOB").unwrap();
        assert_eq!(b.base_address, 0x4002_0400);
        assert_eq!(b.group.as_deref(), Some("GPIO"));
        assert_eq!(b.description, "General-purpose I/Os");
        let moder = b.find_register("MODER").unwrap();
        assert_eq!(moder.address, 0x4002_0400);
        assert_eq!(b.find_register("ODR").unwrap().address, 0x4002_0414);
    }

    #[test]
    fn resolves_derived_registers_and_read_actions() {
        let d = device();
        let usart = d.find_peripheral("USART1").unwrap();
        let copy = usart.find_register("SR_COPY").unwrap();
        assert_eq!(copy.address, 0x4001_1008);
        assert_eq!(copy.fields.len(), 2);
        assert_eq!(copy.reset_value, 0xC0);
        let dr = usart.find_register("DR").unwrap();
        assert!(dr.read_side_effects);
        assert!(!dr.auto_readable());
    }

    #[test]
    fn expands_clusters_and_register_arrays() {
        let d = device();
        let dma = d.find_peripheral("DMA1").unwrap();
        assert_eq!(dma.children.len(), 5);
        let RegisterNode::Cluster(s1) = &dma.children[1] else {
            panic!("expected cluster")
        };
        assert_eq!(s1.name, "S[1]");
        assert_eq!(s1.address, 0x4002_6000 + 0x10 + 0x18);
        let RegisterNode::Register(ndtr) = &s1.children[1] else {
            panic!()
        };
        assert_eq!(ndtr.address, s1.address + 4);
        assert_eq!(ndtr.size, 16);
        let flags: Vec<_> = dma
            .registers()
            .into_iter()
            .filter(|r| r.name.starts_with("FLAG"))
            .map(|r| (r.name.clone(), r.address))
            .collect();
        assert_eq!(
            flags,
            [
                ("FLAGA".to_string(), 0x4002_6100),
                ("FLAGB".to_string(), 0x4002_6104),
                ("FLAGC".to_string(), 0x4002_6108)
            ]
        );
    }

    #[test]
    fn expands_dont_care_enum_values() {
        let d = device();
        let dma = d.find_peripheral("DMA1").unwrap();
        let cr = dma.registers()[0];
        let dir = &cr.fields[0];
        let names: Vec<_> = dir
            .enums
            .iter()
            .map(|e| (e.name.as_str(), e.value))
            .collect();
        assert_eq!(
            names,
            [
                ("P2M", 0),
                ("M2P", 1),
                ("M2M_or_reserved", 2),
                ("M2M_or_reserved", 3)
            ]
        );
    }

    #[test]
    fn field_insert_and_parse_input() {
        let d = device();
        let moder = d
            .find_peripheral("GPIOA")
            .unwrap()
            .find_register("MODER")
            .unwrap();
        let f1 = moder.fields.iter().find(|f| f.name == "MODER1").unwrap();
        assert_eq!(f1.parse_input("output").unwrap(), 1);
        assert_eq!(f1.parse_input("Analog (3)").unwrap(), 3);
        assert_eq!(f1.parse_input("0b10").unwrap(), 2);
        assert!(f1.parse_input("4").is_err());
        assert!(f1.parse_input("bogus").is_err());
        assert_eq!(f1.insert(0xFFFF_FFFF, 0), 0xFFFF_FFF3);
        assert_eq!(f1.insert(0, 3), 0xC);
    }

    #[test]
    fn plans_contiguous_reads_and_skips_unsafe_registers() {
        let d = device();
        let gpio = d.find_peripheral("GPIOA").unwrap();
        // MODER (0x0), IDR (0x10), ODR (0x14); BSRR is write-only.
        assert_eq!(
            read_plan(&gpio.registers()),
            [(0x4002_0000, 4), (0x4002_0010, 8)]
        );
        let usart = d.find_peripheral("USART1").unwrap();
        // DR (0x4) has a read side effect and splits the range.
        assert_eq!(
            read_plan(&usart.registers()),
            [(0x4001_1000, 4), (0x4001_1008, 4)]
        );
    }

    #[test]
    fn rejects_invalid_documents() {
        assert!(Device::parse("<foo/>").is_err());
        assert!(Device::parse("not xml").is_err());
        let bad_derive = r#"<device><peripherals><peripheral derivedFrom="NOPE"><name>X</name><baseAddress>0</baseAddress></peripheral></peripherals></device>"#;
        assert!(Device::parse(bad_derive).is_err());
    }

    #[test]
    fn parses_scaled_integers() {
        assert_eq!(parse_scaled("0x20"), Some(32));
        assert_eq!(parse_scaled("4k"), Some(4096));
        assert_eq!(parse_scaled("#11"), Some(3));
        assert_eq!(parse_scaled("0xFk"), None);
    }
}
