//! 纯 Rust 的 ISO 9660 + Joliet 镜像生成器。
//!
//! 只实现烧录场景需要的最小子集:
//!   * 主卷描述符 (PVD),ISO 9660 Level 1 风格的 8.3 名称
//!   * Joliet 补充卷描述符 (SVD),UCS-2 名称,支持中文
//!   * 目录记录、L/M 路径表
//!
//! 文件数据在两个目录树之间共享——PVD 和 Joliet 只是两套命名和目录块,
//! 数据区只写一份。

use anyhow::{bail, Context, Result};
use std::fs::{self, File};
use std::io::{BufWriter, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SECTOR: u32 = 2048;
const SECTORS_SYSTEM: u32 = 16;

/// 生成结果概要
#[derive(Debug, Clone)]
pub struct IsoSummary {
    pub sectors: u32,
    pub bytes: u64,
    pub files: usize,
    pub dirs: usize,
}

// ------------------------------------------------------------------ 时间

fn iso_datetime_7(t: SystemTime) -> [u8; 7] {
    let (y, mo, d, h, mi, s) = ymdhms(t);
    let year = (y - 1900).clamp(0, 255) as u8;
    [year, mo, d, h, mi, s, 0]
}

fn iso_datetime_17(t: SystemTime) -> [u8; 17] {
    let (y, mo, d, h, mi, s) = ymdhms(t);
    let mut b = [0u8; 17];
    b[0..4].copy_from_slice(format!("{:04}", y).as_bytes());
    b[4] = b'0' + mo / 10;
    b[5] = b'0' + mo % 10;
    b[6] = b'0' + d / 10;
    b[7] = b'0' + d % 10;
    b[8] = b'0' + h / 10;
    b[9] = b'0' + h % 10;
    b[10] = b'0' + mi / 10;
    b[11] = b'0' + mi % 10;
    b[12] = b'0' + s / 10;
    b[13] = b'0' + s % 10;
    b[14] = b'0';
    b[15] = b'0';
    b[16] = 0;
    b
}

fn ymdhms(t: SystemTime) -> (i32, u8, u8, u8, u8, u8) {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    // 简化版公历换算,足够 ISO 使用
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let h = (rem / 3600) as u8;
    let mi = ((rem % 3600) / 60) as u8;
    let s = (rem % 60) as u8;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = if mo <= 2 { y + 1 } else { y } as i32;
    (year, mo, d, h, mi, s)
}

// ------------------------------------------------------------------ 字节序

fn both16(v: u16) -> [u8; 4] {
    let le = v.to_le_bytes();
    let be = v.to_be_bytes();
    [le[0], le[1], be[0], be[1]]
}

fn both32(v: u32) -> [u8; 8] {
    let le = v.to_le_bytes();
    let be = v.to_be_bytes();
    [le[0], le[1], le[2], le[3], be[0], be[1], be[2], be[3]]
}

// ------------------------------------------------------------------ 树

struct Node {
    name: String,
    is_dir: bool,
    src: PathBuf,
    size: u64,
    mtime: SystemTime,
    parent: usize,
    children: Vec<usize>,
    depth: u16,

    /// PVD 里的标识符,例如 "FILE.TXT;1"
    iso_name: Vec<u8>,
    /// Joliet 里的标识符 (UCS-2BE)
    jol_name: Vec<u8>,

    /// 文件数据所在扇区
    data_lba: u32,
    /// 目录在 PVD 树中的扇区
    iso_dir_lba: u32,
    /// 目录在 Joliet 树中的扇区
    jol_dir_lba: u32,
}

struct Tree {
    nodes: Vec<Node>,
}

impl Tree {
    /// 从多个来源建树。目录的内容放到光盘根;文件以其文件名放进根。
    fn collect(srcs: &[PathBuf]) -> Result<Tree> {
        let mut nodes: Vec<Node> = Vec::new();
        let root = Node {
            name: String::new(),
            is_dir: true,
            src: PathBuf::new(),
            size: 0,
            mtime: SystemTime::now(),
            parent: 0,
            children: Vec::new(),
            depth: 0,
            iso_name: Vec::new(),
            jol_name: Vec::new(),
            data_lba: 0,
            iso_dir_lba: 0,
            jol_dir_lba: 0,
        };
        nodes.push(root);

        for src in srcs {
            let meta = fs::metadata(src)
                .with_context(|| format!("无法读取 {}", src.display()))?;
            if meta.is_dir() {
                let mut entries: Vec<_> = fs::read_dir(src)
                    .with_context(|| format!("无法列目录 {}", src.display()))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                entries.sort_by_key(|e| e.file_name());
                for e in entries {
                    add_child(&mut nodes, 0, &e.path(), 1)?;
                }
            } else {
                add_child(&mut nodes, 0, src, 1)?;
            }
        }

        Ok(Tree { nodes })
    }

    fn assign_names(&mut self) -> Result<()> {
        // 根
        self.nodes[0].iso_name = vec![0u8];
        self.nodes[0].jol_name = vec![0u8];

        for i in 0..self.nodes.len() {
            let children = self.nodes[i].children.clone();
            let mut used_iso: Vec<String> = Vec::new();
            let mut used_jol: Vec<String> = Vec::new();
            for c in children {
                let is_dir = self.nodes[c].is_dir;
                let raw = self.nodes[c].name.clone();

                let iso = make_iso_name(&raw, is_dir, &used_iso);
                let jol = make_joliet_name(&raw, &used_jol, is_dir);
                used_iso.push(String::from_utf8_lossy(&iso).to_string());
                used_jol.push(String::from_utf8_lossy(&jol).to_string());
                self.nodes[c].iso_name = iso;
                self.nodes[c].jol_name = jol;
            }
        }

        // 目录块的 ".." 指父目录,根指向自身
        for i in 1..self.nodes.len() {
            let p = self.nodes[i].parent;
            if self.nodes[p].is_dir == false {
                bail!("内部错误:父节点不是目录");
            }
        }
        Ok(())
    }
}

fn add_child(nodes: &mut Vec<Node>, parent: usize, path: &Path, depth: u16) -> Result<()> {
    let meta = fs::metadata(path).with_context(|| format!("无法读取 {}", path.display()))?;
    if meta.is_dir() == false && meta.is_file() == false {
        return Ok(()); // 跳过设备文件、套接字等
    }
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "UNNAMED".to_string());

    let idx = nodes.len();
    nodes.push(Node {
        name: name.clone(),
        is_dir: meta.is_dir(),
        src: path.to_path_buf(),
        size: if meta.is_file() { meta.len() } else { 0 },
        mtime: meta.modified().unwrap_or(SystemTime::now()),
        parent,
        children: Vec::new(),
        depth,
        iso_name: Vec::new(),
        jol_name: Vec::new(),
        data_lba: 0,
        iso_dir_lba: 0,
        jol_dir_lba: 0,
    });
    nodes[parent].children.push(idx);

    if meta.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(path)
            .with_context(|| format!("无法列目录 {}", path.display()))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            add_child(nodes, idx, &e.path(), depth + 1)?;
        }
    }
    Ok(())
}

fn make_iso_name(raw: &str, is_dir: bool, used: &[String]) -> Vec<u8> {
    let mut base: String = raw
        .chars()
        .map(|c| {
            let u = c.to_ascii_uppercase();
            if u.is_ascii_alphanumeric() || u == '_' {
                u
            } else {
                '_'
            }
        })
        .collect();

    let mut ext = String::new();
    if is_dir == false {
        if let Some(pos) = base.rfind('.') {
            let e: String = base[pos + 1..].to_string();
            base.truncate(pos);
            ext = e;
        }
    }
    if base.is_empty() {
        base = "FILE".into();
    }
    base.truncate(8);
    ext.truncate(3);

    let mut candidate = if is_dir {
        base.clone()
    } else if ext.is_empty() {
        format!("{base}.")
    } else {
        format!("{base}.{ext}")
    };

    let mut n = 1;
    while used.iter().any(|u| u == &candidate) {
        let suffix = format!("~{}", n);
        let mut b = base.clone();
        b.truncate(8usize.saturating_sub(suffix.len()));
        candidate = if is_dir {
            format!("{b}{suffix}")
        } else if ext.is_empty() {
            format!("{b}{suffix}.")
        } else {
            format!("{b}{suffix}.{ext}")
        };
        n += 1;
    }

    if is_dir {
        candidate.into_bytes()
    } else {
        format!("{candidate};1").into_bytes()
    }
}

fn make_joliet_name(raw: &str, used: &[String], is_dir: bool) -> Vec<u8> {
    let mut name = raw.to_string();
    // Joliet 标识符最长 64 字节 = 32 个 UCS-2 字符
    if name.chars().count() > 32 {
        name = name.chars().take(32).collect();
    }
    let mut bytes = name.encode_utf16().flat_map(|u| u.to_be_bytes()).collect::<Vec<u8>>();

    let mut n = 1;
    while used
        .iter()
        .any(|u| u.as_bytes() == &bytes[..])
    {
        let suffix = format!("~{}", n);
        let keep = 32usize.saturating_sub(suffix.chars().count());
        let stem: String = name.chars().take(keep).collect();
        let candidate = format!("{stem}{suffix}");
        bytes = candidate
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect::<Vec<u8>>();
        n += 1;
    }

    if bytes.len() > 64 {
        bytes.truncate(64);
    }
    // Joliet 里目录名不带上限差异,文件也不需要 ";1"
    let _ = is_dir;
    bytes
}

// ------------------------------------------------------------------ 目录记录

fn record_len(name_len: usize) -> usize {
    let n = 33 + name_len;
    if n % 2 == 1 { n + 1 } else { n }
}

fn put_dir_record(
    out: &mut Vec<u8>,
    name: &[u8],
    extent: u32,
    size: u32,
    is_dir: bool,
    dt: &[u8; 7],
) {
    let len = record_len(name.len());
    let start = out.len();
    out.resize(start + len, 0);
    let r = &mut out[start..start + len];

    r[0] = len as u8;
    r[1] = 0;
    r[2..10].copy_from_slice(&both32(extent));
    r[10..18].copy_from_slice(&both32(size));
    r[18..25].copy_from_slice(dt);
    r[25] = if is_dir { 0x02 } else { 0x00 };
    r[26] = 0;
    r[27] = 0;
    r[28..32].copy_from_slice(&both16(1));
    r[32] = name.len() as u8;
    r[33..33 + name.len()].copy_from_slice(name);
}

/// 目录块:先 "." 再 ".." 再子项
fn build_dir_block(
    tree: &Tree,
    dir_idx: usize,
    joliet: bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    let node = &tree.nodes[dir_idx];

    let (self_lba, self_size) = if joliet {
        (node.jol_dir_lba, dir_block_size(tree, dir_idx, true))
    } else {
        (node.iso_dir_lba, dir_block_size(tree, dir_idx, false))
    };

    // "."
    put_dir_record(&mut out, &[0u8], self_lba, self_size, true, &iso_datetime_7(node.mtime));

    // ".."
    let parent = if dir_idx == 0 { 0 } else { node.parent };
    let (p_lba, p_size) = if joliet {
        (tree.nodes[parent].jol_dir_lba, dir_block_size(tree, parent, true))
    } else {
        (tree.nodes[parent].iso_dir_lba, dir_block_size(tree, parent, false))
    };
    put_dir_record(&mut out, &[1u8], p_lba, p_size, true, &iso_datetime_7(node.mtime));

    for &c in &node.children {
        let ch = &tree.nodes[c];
        let name = if joliet { &ch.jol_name } else { &ch.iso_name };
        let (extent, size) = if ch.is_dir {
            if joliet {
                (ch.jol_dir_lba, dir_block_size(tree, c, true))
            } else {
                (ch.iso_dir_lba, dir_block_size(tree, c, false))
            }
        } else {
            (ch.data_lba, ch.size as u32)
        };
        put_dir_record(&mut out, name, extent, size, ch.is_dir, &iso_datetime_7(ch.mtime));
    }

    // 补齐到整扇区
    let padded = ((out.len() as u32 + SECTOR - 1) / SECTOR) * SECTOR;
    out.resize(padded as usize, 0);
    out
}

fn dir_block_size(tree: &Tree, dir_idx: usize, joliet: bool) -> u32 {
    let node = &tree.nodes[dir_idx];
    let mut len = 0usize;
    len += record_len(1); // "."
    len += record_len(1); // ".."
    for &c in &node.children {
        let ch = &tree.nodes[c];
        let nl = if joliet { ch.jol_name.len() } else { ch.iso_name.len() };
        len += record_len(nl);
    }
    let padded = ((len as u32 + SECTOR - 1) / SECTOR) * SECTOR;
    padded
}

// ------------------------------------------------------------------ 路径表

fn build_path_table(tree: &Tree, joliet: bool, big_endian: bool) -> Vec<u8> {
    // 按 (depth, 索引) 排序 —— 路径表要求层级顺序
    let mut order: Vec<usize> = (0..tree.nodes.len())
        .filter(|&i| tree.nodes[i].is_dir)
        .collect();
    order.sort_by_key(|&i| (tree.nodes[i].depth, i));

    let mut index_of = vec![0u16; tree.nodes.len()];
    for (n, &i) in order.iter().enumerate() {
        index_of[i] = (n + 1) as u16;
    }

    let mut out = Vec::new();
    for &i in &order {
        let node = &tree.nodes[i];
        let name = if i == 0 {
            Vec::new()
        } else if joliet {
            node.jol_name.clone()
        } else {
            node.iso_name.clone()
        };
        let extent = if joliet { node.jol_dir_lba } else { node.iso_dir_lba };
        let parent = if i == 0 { 0 } else { index_of[node.parent] };

        out.push((1 + name.len()) as u8);
        out.push(0);
        if big_endian {
            out.extend_from_slice(&extent.to_be_bytes());
            out.extend_from_slice(&parent.to_be_bytes());
        } else {
            out.extend_from_slice(&extent.to_le_bytes());
            out.extend_from_slice(&parent.to_le_bytes());
        }
        out.extend_from_slice(&name);
        if (1 + name.len()) % 2 == 1 {
            out.push(0);
        }
    }
    let padded = ((out.len() as u32 + SECTOR - 1) / SECTOR) * SECTOR;
    out.resize(padded as usize, 0);
    out
}

// ------------------------------------------------------------------ 布局

fn layout(tree: &mut Tree) -> Result<(u32, u32, u32, u32, u32)> {
    // 返回 (pvd_pt_size, jol_pt_size, l_pt_lba, m_pt_lba, 下一个可用扇区) 之类
    let n = tree.nodes.len();

    // 先算各目录块大小
    let mut iso_sizes = vec![0u32; n];
    let mut jol_sizes = vec![0u32; n];
    for i in 0..n {
        if tree.nodes[i].is_dir {
            iso_sizes[i] = dir_block_size(tree, i, false);
            jol_sizes[i] = dir_block_size(tree, i, true);
        }
    }

    let mut cursor = SECTORS_SYSTEM + 3; // 16 PVD, 17 SVD, 18 terminator

    // 路径表:PVD L, PVD M, Joliet L, Joliet M
    let pvd_pt = build_path_table(tree, false, false);
    let jol_pt = build_path_table(tree, true, false);
    let pvd_pt_sectors = pvd_pt.len() as u32 / SECTOR;
    let jol_pt_sectors = jol_pt.len() as u32 / SECTOR;

    let pvd_l = cursor;
    cursor += pvd_pt_sectors;
    let pvd_m = cursor;
    cursor += pvd_pt_sectors;
    let jol_l = cursor;
    cursor += jol_pt_sectors;
    let jol_m = cursor;
    cursor += jol_pt_sectors;

    // 目录块(PVD 树)
    for i in 0..n {
        if tree.nodes[i].is_dir {
            tree.nodes[i].iso_dir_lba = cursor;
            cursor += iso_sizes[i] / SECTOR;
        }
    }
    // 目录块(Joliet 树)
    for i in 0..n {
        if tree.nodes[i].is_dir {
            tree.nodes[i].jol_dir_lba = cursor;
            cursor += jol_sizes[i] / SECTOR;
        }
    }
    // 文件数据
    for i in 0..n {
        if tree.nodes[i].is_dir == false {
            tree.nodes[i].data_lba = cursor;
            let sectors = ((tree.nodes[i].size as u32 + SECTOR - 1) / SECTOR).max(1);
            cursor += sectors;
        }
    }

    Ok((pvd_l, pvd_m, jol_l, jol_m, cursor))
}

// ------------------------------------------------------------------ 主入口

/// 生成 ISO。
///
/// `src` 是目录(内容放到光盘根)或单个文件。
/// `progress` 在拷贝数据阶段被调用 `(已写字节, 总字节)`。
pub fn build_iso(
    srcs: &[PathBuf],
    label: &str,
    out: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<IsoSummary> {
    let mut tree = Tree::collect(srcs)?;
    tree.assign_names()?;

    let (pvd_l, pvd_m, jol_l, jol_m, total_sectors) = layout(&mut tree)?;

    let total_bytes: u64 = tree
        .nodes
        .iter()
        .filter(|n| n.is_dir == false)
        .map(|n| n.size)
        .sum();

    // 打开输出
    let mut f = BufWriter::with_capacity(
        1 << 20,
        File::create(out).with_context(|| format!("无法创建 {}", out.display()))?,
    );

    // 系统区
    let mut zero = vec![0u8; SECTOR as usize];
    for _ in 0..SECTORS_SYSTEM {
        f.write_all(&zero)?;
    }

    let vol_id_iso = make_vol_id_ascii(label);
    let vol_id_jol = make_vol_id_joliet(label);

    // PVD
    zero.iter_mut().for_each(|b| *b = 0);
    write_pvd(
        &mut zero,
        1,
        &vol_id_iso,
        total_sectors,
        pvd_l,
        pvd_m,
        &tree,
        &[],
    );
    f.write_all(&zero)?;

    // Joliet SVD
    zero.iter_mut().for_each(|b| *b = 0);
    write_pvd(
        &mut zero,
        2,
        &vol_id_jol,
        total_sectors,
        jol_l,
        jol_m,
        &tree,
        b"%/E",
    );
    f.write_all(&zero)?;

    // Terminator
    zero.iter_mut().for_each(|b| *b = 0);
    zero[0] = 255;
    zero[1..6].copy_from_slice(b"CD001");
    zero[6] = 1;
    f.write_all(&zero)?;

    // 路径表
    f.write_all(&build_path_table(&tree, false, false))?;
    f.write_all(&build_path_table(&tree, false, true))?;
    f.write_all(&build_path_table(&tree, true, false))?;
    f.write_all(&build_path_table(&tree, true, true))?;

    // 目录块
    for i in 0..tree.nodes.len() {
        if tree.nodes[i].is_dir {
            let b = build_dir_block(&tree, i, false);
            f.write_all(&b)?;
        }
    }
    for i in 0..tree.nodes.len() {
        if tree.nodes[i].is_dir {
            let b = build_dir_block(&tree, i, true);
            f.write_all(&b)?;
        }
    }

    // 文件数据
    let mut written: u64 = 0;
    for i in 0..tree.nodes.len() {
        if tree.nodes[i].is_dir {
            continue;
        }
        let node = &tree.nodes[i];
        let mut rf = File::open(&node.src)
            .with_context(|| format!("无法打开 {}", node.src.display()))?;
        let copied = std::io::copy(&mut rf, &mut f)?;
        if copied != node.size {
            bail!(
                "{} 大小在生成过程中变化({} != {})",
                node.src.display(),
                copied,
                node.size
            );
        }
        // 补齐扇区
        let pad = (SECTOR as u64 - (node.size % SECTOR as u64)) % SECTOR as u64;
        if pad > 0 {
            f.write_all(&vec![0u8; pad as usize])?;
        }
        written += copied;
        progress(written, total_bytes);
    }

    // 若没有文件,也补到声明的大小
    let cur = {
        let pos = f.stream_position()?;
        (pos / SECTOR as u64) as u32
    };
    if cur < total_sectors {
        let pad = (total_sectors - cur) as usize * SECTOR as usize;
        f.write_all(&vec![0u8; pad])?;
    }

    f.flush()?;
    let mut file = f.into_inner().map_err(|e| anyhow::anyhow!("{}", e))?;
    file.flush()?;
    file.sync_all()?;

    let dirs = tree.nodes.iter().filter(|n| n.is_dir).count();
    let files = tree.nodes.iter().filter(|n| n.is_dir == false).count();

    Ok(IsoSummary {
        sectors: total_sectors,
        bytes: total_sectors as u64 * SECTOR as u64,
        files,
        dirs,
    })
}

fn make_vol_id_ascii(label: &str) -> [u8; 32] {
    let mut out = [b' '; 32];
    let s: String = label
        .chars()
        .map(|c| {
            let u = c.to_ascii_uppercase();
            if u.is_ascii_alphanumeric() || u == '_' {
                u
            } else {
                '_'
            }
        })
        .take(32)
        .collect();
    for (i, b) in s.bytes().enumerate() {
        out[i] = b;
    }
    out
}

fn make_vol_id_joliet(label: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let s: String = label.chars().take(16).collect();
    let bytes: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
    for (i, b) in bytes.into_iter().take(32).enumerate() {
        out[i] = b;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn write_pvd(
    buf: &mut [u8],
    ty: u8,
    vol_id: &[u8; 32],
    total_sectors: u32,
    l_pt: u32,
    m_pt: u32,
    tree: &Tree,
    escape: &[u8],
) {
    let is_joliet = ty == 2;
    let pt = build_path_table(tree, is_joliet, false);

    buf[0] = ty;
    buf[1..6].copy_from_slice(b"CD001");
    buf[6] = 1;
    // 系统标识符 8..40
    for b in &mut buf[8..40] {
        *b = b' ';
    }
    // 卷标识符 40..72
    buf[40..72].copy_from_slice(vol_id);
    // 卷空间大小 80..88
    buf[80..88].copy_from_slice(&both32(total_sectors));
    // 卷集大小 120..124
    buf[120..124].copy_from_slice(&both16(1));
    // 卷序数 124..128
    buf[124..128].copy_from_slice(&both16(1));
    // 逻辑块大小 128..132
    buf[128..132].copy_from_slice(&both16(SECTOR as u16));
    // 路径表大小 132..140
    buf[132..140].copy_from_slice(&both32(pt.len() as u32));
    // L 路径表 140..144
    buf[140..144].copy_from_slice(&l_pt.to_le_bytes());
    // 可选 L 147.. 空
    // M 路径表 148..152
    buf[148..152].copy_from_slice(&m_pt.to_be_bytes());
    // 可选 M 空

    // 根目录记录 156..190
    let root_name: &[u8] = &[0u8];
    let (root_lba, root_size) = if is_joliet {
        (tree.nodes[0].jol_dir_lba, dir_block_size(tree, 0, true))
    } else {
        (tree.nodes[0].iso_dir_lba, dir_block_size(tree, 0, false))
    };
    let mut rec = Vec::new();
    put_dir_record(
        &mut rec,
        root_name,
        root_lba,
        root_size,
        true,
        &iso_datetime_7(SystemTime::now()),
    );
    // 根记录固定 34 字节
    let n = rec.len().min(34);
    buf[156..156 + n].copy_from_slice(&rec[..n]);

    // 标识符串
    for b in &mut buf[190..318] {
        *b = b' ';
    }
    for b in &mut buf[318..446] {
        *b = b' ';
    }
    for b in &mut buf[446..574] {
        *b = b' ';
    }
    for b in &mut buf[574..702] {
        *b = b' ';
    }
    for b in &mut buf[702..739] {
        *b = b' ';
    }
    for b in &mut buf[739..776] {
        *b = b' ';
    }
    for b in &mut buf[776..813] {
        *b = b' ';
    }
    buf[574..574 + 16].copy_from_slice(b"IS903BURN       ");

    // 日期
    let now = SystemTime::now();
    buf[813..830].copy_from_slice(&iso_datetime_17(now));
    buf[830..847].copy_from_slice(&iso_datetime_17(now));
    for b in &mut buf[847..864] {
        *b = b'0';
    }
    buf[864] = 0;
    for b in &mut buf[864..881] {
        *b = b'0';
    }
    buf[881] = 1; // 文件结构版本

    if escape.is_empty() == false {
        buf[88..88 + escape.len()].copy_from_slice(escape);
    }
}
