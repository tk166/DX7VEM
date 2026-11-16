//! 弓法（M2 会在这里加状态机：换弓瞬态、tremolo 时序、glissando 滑音）。

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Articulation {
    Arco,
    Marcato,
    Glissando,
    Spiccato,
    Pizzicato,
    ColLegno,
    Tremolo,
}

impl Articulation {
    /// C ABI 的整型编号，顺序与文档/UI 一致。
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::Marcato,
            2 => Self::Glissando,
            3 => Self::Spiccato,
            4 => Self::Pizzicato,
            5 => Self::ColLegno,
            6 => Self::Tremolo,
            _ => Self::Arco,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Arco => "arco",
            Self::Marcato => "marcato",
            Self::Glissando => "glissando",
            Self::Spiccato => "spiccato",
            Self::Pizzicato => "pizzicato",
            Self::ColLegno => "collegno",
            Self::Tremolo => "tremolo",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "arco" => Self::Arco,
            "marcato" => Self::Marcato,
            "glissando" | "gliss" => Self::Glissando,
            "spiccato" => Self::Spiccato,
            "pizzicato" | "pizz" => Self::Pizzicato,
            "collegno" | "col-legno" => Self::ColLegno,
            "tremolo" | "trem" => Self::Tremolo,
            _ => return None,
        })
    }
}
