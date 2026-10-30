//! Just enough of the safetensors format to read one 2-D tensor: an 8-byte little-endian header
//! length, a JSON header (`{"name": {"dtype", "shape", "data_offsets"}, "__metadata__"?}`), then
//! the data. F32, F16 and BF16 become f32.

/// A matrix, row major.
#[derive(Debug, Clone)]
pub struct Matrix {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f32>,
}

impl Matrix {
    pub fn row(&self, i: usize) -> Option<&[f32]> {
        self.data.get(i * self.cols..(i + 1) * self.cols)
    }
}

/// The tensor `name` of the file's bytes (or its only tensor, when it has one).
pub fn read(bytes: &[u8], name: &str) -> Result<Matrix, String> {
    let len = bytes
        .get(..8)
        .and_then(|b| b.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or("too short for a safetensors file")? as usize;
    let header = bytes.get(8..8usize.saturating_add(len)).ok_or("its header is cut off")?;
    let header: serde_json::Value =
        serde_json::from_slice(header).map_err(|e| format!("its header isn't JSON: {e}"))?;
    let tensors = header.as_object().ok_or("its header isn't a table")?;
    let mut named = tensors.iter().filter(|(k, _)| *k != "__metadata__");
    let t = match tensors.get(name) {
        Some(t) => t,
        None => match (named.next(), named.next()) {
            (Some((_, t)), None) => t,
            _ => return Err(format!("it has no tensor named {name:?}")),
        },
    };
    let shape: Vec<usize> = t["shape"]
        .as_array()
        .ok_or("a tensor has no shape")?
        .iter()
        .filter_map(|v| v.as_u64().map(|v| v as usize))
        .collect();
    let [rows, cols] = shape[..] else {
        return Err(format!("the tensor is {}-D, not a matrix", shape.len()));
    };
    let offs: Vec<usize> = t["data_offsets"]
        .as_array()
        .ok_or("a tensor has no data_offsets")?
        .iter()
        .filter_map(|v| v.as_u64().map(|v| v as usize))
        .collect();
    let [from, to] = offs[..] else { return Err("bad data_offsets".into()) };
    let data_at = 8 + len;
    let raw = bytes
        .get(data_at + from..data_at + to)
        .ok_or("the tensor's data is past the end of the file")?;
    let n = rows.checked_mul(cols).ok_or("the tensor is too large")?;
    let data: Vec<f32> = match t["dtype"].as_str() {
        Some("F32") if raw.len() == n * 4 => {
            raw.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
        }
        Some("F16") if raw.len() == n * 2 => {
            raw.as_chunks::<2>().0.iter().map(|b| f16_to_f32(u16::from_le_bytes(*b))).collect()
        }
        Some("BF16") if raw.len() == n * 2 => raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| f32::from_bits(u32::from(u16::from_le_bytes(*b)) << 16))
            .collect(),
        Some(d @ ("F32" | "F16" | "BF16")) => {
            return Err(format!("a {rows}×{cols} {d} tensor can't be {} bytes", raw.len()));
        }
        d => return Err(format!("its dtype {d:?} isn't one ptyro reads (F32, F16, BF16)")),
    };
    Ok(Matrix { rows, cols, data })
}

/// IEEE half to single precision.
fn f16_to_f32(h: u16) -> f32 {
    let sign = u32::from(h >> 15) << 31;
    let exp = u32::from((h >> 10) & 0x1f);
    let frac = u32::from(h & 0x3ff);
    let bits = match (exp, frac) {
        (0, 0) => sign,
        // Subnormal: normalize it.
        (0, f) => {
            let shift = f.leading_zeros() - 21;
            sign | ((127 - 15 + 1 - shift) << 23) | ((f << shift) & 0x3ff) << 13
        }
        (0x1f, f) => sign | 0x7f80_0000 | (f << 13),
        (e, f) => sign | ((e + 127 - 15) << 23) | (f << 13),
    };
    f32::from_bits(bits)
}

/// A safetensors file of one F32 matrix (the synthetic test models; [`read`]'s inverse).
pub fn write_f32(name: &str, m: &Matrix) -> Vec<u8> {
    let bytes = m.data.len() * 4;
    let header = serde_json::json!({
        name: { "dtype": "F32", "shape": [m.rows, m.cols], "data_offsets": [0, bytes] }
    })
    .to_string();
    let mut out = Vec::with_capacity(8 + header.len() + bytes);
    out.extend_from_slice(&(header.len() as u64).to_le_bytes());
    out.extend_from_slice(header.as_bytes());
    for v in &m.data {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x3555), 0.333_251_95);
        assert_eq!(f16_to_f32(0x0001), 5.960_464_5e-8);
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert!(f16_to_f32(0x7c00).is_infinite());
    }

    #[test]
    fn round_trips_and_reads_each_dtype() {
        let m = Matrix { rows: 2, cols: 3, data: vec![1.0, -2.0, 0.5, 0.0, 3.0, 4.0] };
        let b = write_f32("embeddings", &m);
        let r = read(&b, "embeddings").unwrap();
        assert_eq!((r.rows, r.cols, r.data.clone()), (2, 3, m.data.clone()));
        assert_eq!(r.row(1), Some(&[0.0, 3.0, 4.0][..]));
        // The only tensor is read whatever its name.
        assert!(read(&b, "other").is_ok());

        let f16 = |dtype: &str, words: &[u16]| {
            let h = format!(
                r#"{{"__metadata__":{{"format":"pt"}},"e":{{"dtype":"{dtype}","shape":[1,2],"data_offsets":[0,4]}}}}"#
            );
            let mut b = (h.len() as u64).to_le_bytes().to_vec();
            b.extend_from_slice(h.as_bytes());
            for w in words {
                b.extend_from_slice(&w.to_le_bytes());
            }
            b
        };
        assert_eq!(read(&f16("F16", &[0x3c00, 0xc000]), "e").unwrap().data, [1.0, -2.0]);
        assert_eq!(read(&f16("BF16", &[0x3f80, 0xc000]), "e").unwrap().data, [1.0, -2.0]);
        assert!(read(&f16("I8", &[0, 0]), "e").unwrap_err().contains("dtype"));
        assert!(read(&b[..20], "embeddings").is_err());
        assert!(read(&b[..b.len() - 1], "embeddings").is_err());
    }
}
