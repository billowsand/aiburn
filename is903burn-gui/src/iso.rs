//! 纯 Rust 的 ISO 9660 + Joliet 镜像生成器。
//!
//! 只实现烧录场景需要的最小子集:
//!   * 主卷描述符 (PVD),ISO 9660 Level 1 风格的 8.3 名称
//!   * Joliet 补充卷描述符 (SVD),UCS-2 名称,支持中文
//!   * 目录记录、L/M 路径表
//!
//! 文件数据在两个目录树之间共享——PVD 和 Joliet 只是两套命名和目录块,
//! 数据区只写一份。

use anyhow::{bail, ensure, Context, Result};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SECTOR: u32 = 2048;
const SECTORS_SYSTEM: u32 = 16;

/// Joliet 标识符上限。
///
/// Joliet 规范写的是 32 个 UCS-2 码元(64 字节)，但 32 个字符对中文长文件名
/// 太短(「XX公司2024年产品规格说明书(正式版).pdf」就已经超过)。Windows CDFS、
/// Linux isofs、macOS 实际都接受 64 个字符 = 128 字节，genisoimage 的默认值
/// 也是 64 个字符，这里采用同一上限。
pub const JOLIET_MAX_CHARS: usize = 64;

/// ISO 9660 允许的目录层级(根目录算第 1 层)
const MAX_DEPTH: u16 = 8;

/// 生成结果概要
#[derive(Debug, Clone)]
pub struct IsoSummary {
    pub sectors: u32,
    pub bytes: u64,
    pub files: usize,
    pub dirs: usize,
    /// 生成过程中需要提示用户的问题(例如文件名被截断)
    pub warnings: Vec<String>,
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
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
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
            let meta =
                fs::symlink_metadata(src).with_context(|| format!("无法读取 {}", src.display()))?;
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

    fn assign_names(&mut self) -> Result<Vec<String>> {
        // 根
        self.nodes[0].iso_name = vec![0u8];
        self.nodes[0].jol_name = vec![0u8];

        let mut warnings = Vec::new();
        for i in 0..self.nodes.len() {
            let children = self.nodes[i].children.clone();
            let mut used_iso: Vec<String> = Vec::new();
            let mut used_jol = Vec::new();
            for c in children {
                let is_dir = self.nodes[c].is_dir;
                let raw = self.nodes[c].name.clone();

                let iso = make_iso_name(&raw, is_dir, &used_iso);
                let (jol, truncated) = make_joliet_name(&raw, &used_jol);
                // PVD 里文件标识符带 ";1" 版本号，比较时也必须带上，
                // 否则同名的两个文件会生成重复的目录记录。
                used_iso.push(String::from_utf8_lossy(&iso).into_owned());
                used_jol.push(jol.clone());
                self.nodes[c].iso_name = iso;
                self.nodes[c].jol_name = jol;
                if truncated {
                    warnings.push(format!(
                        "文件名超过 {JOLIET_MAX_CHARS} 个字符，光盘上将被截断：{raw}"
                    ));
                }
            }
        }

        // 目录块的 ".." 指父目录,根指向自身
        for i in 1..self.nodes.len() {
            let p = self.nodes[i].parent;
            if self.nodes[p].is_dir == false {
                bail!("内部错误:父节点不是目录");
            }
        }
        Ok(warnings)
    }
}

fn add_child(nodes: &mut Vec<Node>, parent: usize, path: &Path, depth: u16) -> Result<()> {
    ensure!(
        depth < MAX_DEPTH,
        "目录层级超过 ISO 9660 的 {MAX_DEPTH} 层限制：{}",
        path.display()
    );
    let meta =
        fs::symlink_metadata(path).with_context(|| format!("无法读取 {}", path.display()))?;
    if meta.is_symlink() {
        bail!("请添加符号链接指向的实际文件或目录：{}", path.display());
    }
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
    let (stem, extension) = if !is_dir {
        raw.rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .unwrap_or((raw, ""))
    } else {
        (raw, "")
    };
    let sanitize = |part: &str| -> String {
        part.chars()
            .map(|c| {
                let upper = c.to_ascii_uppercase();
                if upper.is_ascii_alphanumeric() || upper == '_' {
                    upper
                } else {
                    '_'
                }
            })
            .collect()
    };
    let mut base = sanitize(stem);
    let mut ext = sanitize(extension);
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

    // 目录没有版本号，文件标识符必须带 ";1"；比较时统一用最终写入的标识符
    let identifier = |name: &str| {
        if is_dir {
            name.to_owned()
        } else {
            format!("{name};1")
        }
    };

    let mut n = 1;
    while used.iter().any(|u| *u == identifier(&candidate)) {
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

    identifier(&candidate).into_bytes()
}

fn make_joliet_name(raw: &str, used: &[Vec<u8>]) -> (Vec<u8>, bool) {
    // Windows 会吞掉末尾的点和空格，先去掉再截断，避免「名字看着短了」。
    let mut chars: Vec<char> = raw
        .chars()
        .map(|c| {
            if u16::try_from(c as u32).is_ok() {
                c
            } else {
                '_'
            }
        })
        .collect();
    while matches!(chars.last(), Some(' ' | '.')) {
        chars.pop();
    }
    if chars.is_empty() {
        chars.push('_');
    }

    let truncated = chars.len() > JOLIET_MAX_CHARS;
    let name: Vec<u16> = chars
        .iter()
        .take(JOLIET_MAX_CHARS)
        .map(|c| *c as u16)
        .collect();

    let mut candidate: Vec<u8> = name.iter().flat_map(|u| u.to_be_bytes()).collect();
    let mut n = 1;
    while used.contains(&candidate) {
        let suffix = format!("~{n}");
        candidate = name
            .iter()
            .copied()
            .take(JOLIET_MAX_CHARS.saturating_sub(suffix.len()))
            .chain(suffix.encode_utf16())
            .flat_map(u16::to_be_bytes)
            .collect();
        n += 1;
    }
    (candidate, truncated)
}

// ------------------------------------------------------------------ 目录记录

fn record_len(name_len: usize) -> usize {
    let n = 33 + name_len;
    if n % 2 == 1 {
        n + 1
    } else {
        n
    }
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
    let start = dir_record_start(out.len(), len);
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
fn build_dir_block(tree: &Tree, dir_idx: usize, joliet: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let node = &tree.nodes[dir_idx];

    let (self_lba, self_size) = if joliet {
        (node.jol_dir_lba, dir_block_size(tree, dir_idx, true))
    } else {
        (node.iso_dir_lba, dir_block_size(tree, dir_idx, false))
    };

    // "."
    put_dir_record(
        &mut out,
        &[0u8],
        self_lba,
        self_size,
        true,
        &iso_datetime_7(node.mtime),
    );

    // ".."
    let parent = if dir_idx == 0 { 0 } else { node.parent };
    let (p_lba, p_size) = if joliet {
        (
            tree.nodes[parent].jol_dir_lba,
            dir_block_size(tree, parent, true),
        )
    } else {
        (
            tree.nodes[parent].iso_dir_lba,
            dir_block_size(tree, parent, false),
        )
    };
    put_dir_record(
        &mut out,
        &[1u8],
        p_lba,
        p_size,
        true,
        &iso_datetime_7(node.mtime),
    );

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
        put_dir_record(
            &mut out,
            name,
            extent,
            size,
            ch.is_dir,
            &iso_datetime_7(ch.mtime),
        );
    }

    // 补齐到整扇区
    let padded = ((out.len() as u32 + SECTOR - 1) / SECTOR) * SECTOR;
    out.resize(padded as usize, 0);
    out
}

fn dir_record_start(offset: usize, len: usize) -> usize {
    let sector = SECTOR as usize;
    if offset % sector + len > sector {
        offset.next_multiple_of(sector)
    } else {
        offset
    }
}

fn dir_block_size(tree: &Tree, dir_idx: usize, joliet: bool) -> u32 {
    let node = &tree.nodes[dir_idx];
    let mut len = 0usize;
    len += record_len(1); // "."
    len += record_len(1); // ".."
    for &c in &node.children {
        let ch = &tree.nodes[c];
        let nl = if joliet {
            ch.jol_name.len()
        } else {
            ch.iso_name.len()
        };
        let record = record_len(nl);
        len = dir_record_start(len, record) + record;
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
        let extent = if joliet {
            node.jol_dir_lba
        } else {
            node.iso_dir_lba
        };
        // 根目录的父目录编号固定为 1(= 它自己)
        let parent = if i == 0 { 1 } else { index_of[node.parent] };

        let name_len = name.len().max(1);
        out.push(name_len as u8);
        out.push(0);
        if big_endian {
            out.extend_from_slice(&extent.to_be_bytes());
            out.extend_from_slice(&parent.to_be_bytes());
        } else {
            out.extend_from_slice(&extent.to_le_bytes());
            out.extend_from_slice(&parent.to_le_bytes());
        }
        if name.is_empty() {
            out.push(0);
        } else {
            out.extend_from_slice(&name);
        }
        if name_len % 2 == 1 {
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
            let size = u32::try_from(tree.nodes[i].size).with_context(|| {
                format!(
                    "单个文件超过 ISO 9660 大小上限：{}",
                    tree.nodes[i].src.display()
                )
            })?;
            let sectors = size.div_ceil(SECTOR).max(1);
            cursor = cursor
                .checked_add(sectors)
                .context("ISO 总容量超过寻址上限")?;
        }
    }

    Ok((pvd_l, pvd_m, jol_l, jol_m, cursor))
}

// ------------------------------------------------------------------ 主入口

/// 生成 ISO。
///
/// `src` 是目录(内容放到光盘根)或单个文件。
/// `progress` 在拷贝数据阶段被调用 `(已写字节, 总字节)`，返回 `false` 表示取消。
pub fn build_iso(
    srcs: &[PathBuf],
    label: &str,
    out: &Path,
    max_bytes: u64,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<IsoSummary> {
    ensure!(!srcs.is_empty(), "请添加要烧录的文件或文件夹");
    let mut tree = Tree::collect(srcs)?;
    let warnings = tree.assign_names()?;

    let (pvd_l, pvd_m, jol_l, jol_m, total_sectors) = layout(&mut tree)?;
    let bytes = u64::from(total_sectors) * u64::from(SECTOR);
    if bytes > max_bytes {
        bail!(
            "ISO 需要 {}，设备容量为 {}",
            crate::device::human_size(bytes),
            crate::device::human_size(max_bytes)
        );
    }

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
    let mut buffer = vec![0; 1024 * 1024];
    for i in 0..tree.nodes.len() {
        if tree.nodes[i].is_dir {
            continue;
        }
        let node = &tree.nodes[i];
        let mut rf =
            File::open(&node.src).with_context(|| format!("无法打开 {}", node.src.display()))?;
        let mut remaining = node.size;
        while remaining > 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            rf.read_exact(&mut buffer[..count])
                .with_context(|| format!("读取 {} 失败", node.src.display()))?;
            f.write_all(&buffer[..count])
                .with_context(|| format!("写入镜像失败（{} 所在磁盘空间不足？）", out.display()))?;
            remaining -= count as u64;
            written += count as u64;
            if !progress(written, total_bytes) {
                bail!("已取消烧录");
            }
        }
        if rf.read(&mut buffer[..1])? != 0 {
            bail!("{} 在生成过程中变大", node.src.display());
        }
        // 补齐扇区
        let sector = u64::from(SECTOR);
        let pad = node.size.next_multiple_of(sector).max(sector) - node.size;
        if pad > 0 {
            f.write_all(&vec![0u8; pad as usize])?;
        }
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
        bytes,
        files,
        dirs,
        warnings,
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
    for pair in out.chunks_exact_mut(2) {
        pair[1] = b' ';
    }
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
    let pt_size: usize = tree
        .nodes
        .iter()
        .filter(|n| n.is_dir)
        .map(|n| {
            let length = if is_joliet {
                n.jol_name.len()
            } else {
                n.iso_name.len()
            };
            8 + length.max(1).next_multiple_of(2)
        })
        .sum();

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
    buf[132..140].copy_from_slice(&both32(pt_size as u32));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn joliet(name: &str, used: &[Vec<u8>]) -> (String, bool) {
        let (bytes, truncated) = make_joliet_name(name, used);
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        (String::from_utf16(&units).unwrap(), truncated)
    }

    #[test]
    fn joliet_keeps_long_chinese_names() {
        // 29 个字符，以前会被截断到 32 字符以内之外的边界上
        let name = "中兴微电子_产品规格说明书_2024年正式版_v2.pdf";
        let (kept, truncated) = joliet(name, &[]);
        assert!(!truncated);
        assert_eq!(kept, name);
    }

    #[test]
    fn joliet_limit_is_64_chars() {
        let name = "文".repeat(JOLIET_MAX_CHARS);
        let (kept, truncated) = joliet(&name, &[]);
        assert!(!truncated);
        assert_eq!(kept.chars().count(), JOLIET_MAX_CHARS);

        let longer = "文".repeat(JOLIET_MAX_CHARS + 3);
        let (kept, truncated) = joliet(&longer, &[]);
        assert!(truncated);
        assert_eq!(kept.chars().count(), JOLIET_MAX_CHARS);
    }

    #[test]
    fn joliet_avoids_trailing_dot_or_space() {
        let (kept, _) = joliet("README.", &[]);
        assert_eq!(kept, "README");
        let (kept, _) = joliet("  名字  ", &[]);
        assert_eq!(kept, "  名字");
    }

    #[test]
    fn joliet_dedupes_truncated_names() {
        let long = "a".repeat(JOLIET_MAX_CHARS + 10);
        let (first, _) = joliet(&long, &[]);
        let first_bytes = make_joliet_name(&long, &[]).0;
        let (second, _) = joliet(&long, &[first_bytes]);
        assert_ne!(first, second);
        assert!(second.ends_with("~1"));
        assert!(second.chars().count() <= JOLIET_MAX_CHARS);
    }

    #[test]
    fn pvd_identifiers_stay_unique() {
        // 两个不同的中文文件名会落到同一个 8.3 名字上，必须自动加 ~1
        let used = Vec::new();
        let a = make_iso_name("中兴报告.pdf", false, &used);
        let used = vec![String::from_utf8_lossy(&a).into_owned()];
        let b = make_iso_name("产品说明.pdf", false, &used);
        assert_ne!(a, b);
        assert!(String::from_utf8_lossy(&b).ends_with(";1"));
    }

    #[test]
    fn joliet_replaces_non_bmp() {
        let (kept, _) = joliet("表情😀.txt", &[]);
        assert_eq!(kept, "表情_.txt");
    }
}
