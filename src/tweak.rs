use crate::pck;
use anyhow::{anyhow, Context, Result};
use binrw::BinRead;
use cfg_if::cfg_if;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom};

#[cfg(not(feature = "gui"))]
use anyhow::bail;
#[cfg(not(feature = "gui"))]
use std::path::PathBuf;

cfg_if! {
    if #[cfg(feature = "gui")] {
        use rust_embed::RustEmbed;

        #[derive(RustEmbed)]
        #[folder = "assets"]
        struct EmbeddedAssets;

        struct EmbeddedSource;

        impl AssetSource for EmbeddedSource {
            fn get_file(&self, relative_path: &str) -> Result<Vec<u8>> {
                let asset = EmbeddedAssets::get(relative_path)
                    .ok_or_else(|| anyhow!("嵌入资源缺失: {}", relative_path))?;
                Ok(asset.data.to_vec())
            }

            fn config_content(&self) -> Result<Cow<'static, str>> {
                let config = EmbeddedAssets::get("replace.toml")
                    .ok_or_else(|| anyhow!("嵌入资源中缺少 replace.toml"))?;
                let content = std::str::from_utf8(&config.data)
                    .context("无法解析 replace.toml 为 UTF-8")?;
                Ok(Cow::Owned(content.to_string()))
            }
        }

        pub fn tweak_game_gde(file_path: &str) -> Result<()> {
            let source = EmbeddedSource;
            run_tweak(file_path, &source)
        }
    } else {
        struct FileSystemSource {
            base_path: PathBuf,
        }

        impl AssetSource for FileSystemSource {
            fn get_file(&self, relative_path: &str) -> Result<Vec<u8>> {
                let full_path = if relative_path.starts_with("../") {
                    self.base_path.join(relative_path.trim_start_matches("../"))
                } else if relative_path.starts_with("./") {
                    self.base_path.join(relative_path.trim_start_matches("./"))
                } else {
                    self.base_path.join(relative_path)
                };

                if !full_path.exists() {
                    bail!("资产文件不存在: {}", full_path.display());
                }

                std::fs::read(&full_path)
                    .with_context(|| format!("无法读取资产文件: {}", full_path.display()))
            }

            fn config_content(&self) -> Result<Cow<'static, str>> {
                let config_path = self.base_path.join("replace.toml");
                let config_str = std::fs::read_to_string(&config_path)
                    .with_context(|| format!("无法读取 replace.toml: {}", config_path.display()))?;
                Ok(Cow::Owned(config_str))
            }
        }

        pub fn tweak_game_gde(file_path: &str, assets_path: &str) -> Result<()> {
            let source = FileSystemSource {
                base_path: PathBuf::from(assets_path),
            };
            run_tweak(file_path, &source)
        }
    }
}

#[derive(Debug, Clone)]
struct VersionConfig {
    version_hashes: HashMap<String, String>,
    required_game_version: String,
    plugin_version: String,
}

trait AssetSource {
    fn get_file(&self, relative_path: &str) -> Result<Vec<u8>>;
    fn config_content(&self) -> Result<Cow<'static, str>>;
}

fn run_tweak<S: AssetSource>(file_path: &str, source: &S) -> Result<()> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(file_path)
        .with_context(|| format!("修改失败，无法打开文件: {}", file_path))?;

    println!("正在读取 PCK 文件头与索引...");
    let (header, index) = pck::read_header_and_index(&mut file)
        .with_context(|| format!("修改失败，读取 PCK 头与索引失败: {}", file_path))?;

    println!("正在加载版本配置...");
    let config_content = source
        .config_content()
        .with_context(|| format!("修改失败，无法读取 replace.toml: {}", file_path))?;
    let version_config = parse_version_config(config_content.as_ref())
        .with_context(|| format!("修改失败，加载版本配置失败: {}", file_path))?;
    println!(
        "✓ 版本配置加载成功，要求游戏版本: {}",
        version_config.required_game_version
    );

    println!("正在校验版本信息...");
    let has_plugin_version = check_plugin_version_txt(&mut file, &header, &index, &version_config)
        .with_context(|| format!("修改失败，版本校验失败: {}", file_path))?;

    if !has_plugin_version {
        println!("未检测到 plugin_version.txt，正在校验 Game.gde 哈希...");
        check_game_gde_hash(&mut file, &header, &index, &version_config)
            .with_context(|| format!("修改失败，哈希校验失败: {}", file_path))?;
    }

    println!("正在加载替换配置...");
    let (mut replacements_owned, delete_list) =
        parse_config(config_content.as_ref(), |asset_path| {
            source.get_file(asset_path)
        })
        .context("加载 replace.toml 失败")?;
    println!(
        "✓ 替换配置加载成功，{} 个文件待注入",
        replacements_owned.len()
    );

    if !delete_list.is_empty() {
        pck::delete_files_in_pck(
            &mut file,
            &header,
            &index,
            delete_list.iter().map(|s| s.as_str()).collect(),
        )
        .context("删除指定文件失败")?;
        println!("✓ 已删除 {} 个指定文件", delete_list.len());
    }

    let (header, index) = pck::read_header_and_index(&mut file).context("删除后重读 PCK 失败")?;

    let plugin_version_content = create_plugin_version_content(&version_config);
    println!(
        "✓ 准备注入 plugin_version.txt (版本: {})",
        version_config.plugin_version
    );
    replacements_owned.push((
        "res://plugin_version.txt".to_string(),
        plugin_version_content,
    ));

    let replacements: Vec<(&str, &[u8])> = replacements_owned
        .iter()
        .map(|(path, data)| (path.as_str(), data.as_slice()))
        .collect();

    pck::replace_files_in_pck(&mut file, &header, &index, replacements)
        .context("写入/替换 PCK 文件失败")?;

    println!("✅ 所有修改已完成！");
    Ok(())
}

fn parse_version_config(config_str: &str) -> Result<VersionConfig> {
    let table: toml::value::Table = toml::from_str(config_str).context("解析 replace.toml 失败")?;

    let version_table = table
        .get("version")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow!("replace.toml 缺少 [version] 表"))?;

    let required_game_version = version_table
        .get("required-game-version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("replace.toml 缺少 required-game-version 字段"))?
        .to_string();

    let plugin_version = version_table
        .get("plugin-version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("replace.toml 缺少 plugin-version 字段"))?
        .to_string();

    let version_hash_table = table
        .get("version-hash")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow!("replace.toml 缺少 [version-hash] 表"))?;

    let mut version_hashes = HashMap::new();
    for (version, hash_value) in version_hash_table {
        let hash_str = hash_value
            .as_str()
            .ok_or_else(|| anyhow!("[version-hash] 中的值必须是字符串: {}", version))?;
        version_hashes.insert(version.clone(), hash_str.to_string());
    }

    Ok(VersionConfig {
        version_hashes,
        required_game_version,
        plugin_version,
    })
}

fn parse_config<F>(
    config_str: &str,
    mut load_asset: F,
) -> Result<(Vec<(String, Vec<u8>)>, Vec<String>)>
where
    F: FnMut(&str) -> Result<Vec<u8>>,
{
    let table: toml::value::Table = toml::from_str(config_str).context("解析 replace.toml 失败")?;

    let replace_table = table
        .get("replace")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow!("replace.toml 缺少 [replace] 表"))?;

    let mut replacements = Vec::with_capacity(replace_table.len());
    for (res_path, asset_value) in replace_table {
        let asset_path = asset_value
            .as_str()
            .ok_or_else(|| anyhow!("[replace] 中的值必须是字符串: {}", res_path))?;

        let asset_data = load_asset(asset_path)?;
        replacements.push((res_path.clone(), asset_data));
    }

    let delete_list = table
        .get("delete")
        .map(|v| {
            if let Some(arr) = v.as_array() {
                arr.iter()
                    .map(|val| {
                        val.as_str()
                            .ok_or_else(|| anyhow!("delete 数组元素必须是字符串"))
                            .map(|s| s.to_string())
                    })
                    .collect::<Result<Vec<String>>>()
            } else if let Some(t) = v.as_table() {
                let arr = t
                    .get("paths")
                    .ok_or_else(|| anyhow!("delete 表需要 paths 数组"))?
                    .as_array()
                    .ok_or_else(|| anyhow!("delete.paths 必须是数组"))?;
                arr.iter()
                    .map(|val| {
                        val.as_str()
                            .ok_or_else(|| anyhow!("delete.paths 元素必须是字符串"))
                            .map(|s| s.to_string())
                    })
                    .collect::<Result<Vec<String>>>()
            } else {
                Err(anyhow!("delete 必须是数组或包含 paths 的表"))
            }
        })
        .transpose()?
        .unwrap_or_default();

    Ok((replacements, delete_list))
}

fn compute_file_hash(data: &[u8]) -> String {
    format!("{:x}", md5::compute(data))
}

fn read_file_from_pck(
    pck_file: &mut std::fs::File,
    _header: &pck::Header,
    entry_offsets: &HashMap<String, u64>,
    res_path: &str,
) -> Result<Vec<u8>> {
    let entry_offset = entry_offsets
        .get(res_path)
        .ok_or_else(|| anyhow!("PCK 中不存在文件: {}", res_path))?;

    let mut reader = std::io::BufReader::new(pck_file.try_clone()?);
    reader
        .seek(SeekFrom::Start(*entry_offset))
        .with_context(|| format!("无法定位文件 entry: {}", res_path))?;

    let entry = pck::RawFileEntry::read(&mut reader)
        .with_context(|| format!("无法读取文件 entry: {}", res_path))?;

    let mut data = vec![0u8; entry.size as usize];
    reader
        .seek(SeekFrom::Start(entry.offset))
        .with_context(|| format!("无法定位文件数据: {}", res_path))?;
    reader
        .read_exact(&mut data)
        .with_context(|| format!("无法读取文件数据: {}", res_path))?;

    Ok(data)
}

fn check_plugin_version_txt(
    pck_file: &mut std::fs::File,
    header: &pck::Header,
    entry_offsets: &HashMap<String, u64>,
    version_config: &VersionConfig,
) -> Result<bool> {
    let plugin_version_path = "res://plugin_version.txt";

    if entry_offsets.contains_key(plugin_version_path) {
        let content = read_file_from_pck(pck_file, header, entry_offsets, plugin_version_path)?;
        let content_str =
            String::from_utf8(content).context("plugin_version.txt 内容无法解析为 UTF-8")?;

        let lines: Vec<&str> = content_str.lines().collect();
        if lines.len() < 2 {
            anyhow::bail!(
                "plugin_version.txt 格式错误，至少需要两行：game-version 和 plugin-version"
            );
        }

        let game_version = lines[0].trim();
        let _existing_plugin_version = lines[1].trim();

        if game_version != version_config.required_game_version {
            anyhow::bail!(
                "游戏版本不匹配，当前已注入版本: {}，当前插件适用于游戏版本: {}",
                game_version,
                version_config.required_game_version
            );
        }

        println!(
            "✓ 已检测到 plugin_version.txt，游戏版本校验通过: {}",
            game_version
        );
        return Ok(true);
    }

    Ok(false)
}

fn check_game_gde_hash(
    pck_file: &mut std::fs::File,
    header: &pck::Header,
    entry_offsets: &HashMap<String, u64>,
    version_config: &VersionConfig,
) -> Result<()> {
    let game_gde_path = "res://Core/Game.gde";

    let game_gde_data = read_file_from_pck(pck_file, header, entry_offsets, game_gde_path)?;
    let current_hash = compute_file_hash(&game_gde_data);

    let expected_hash = version_config
        .version_hashes
        .get(&version_config.required_game_version)
        .ok_or_else(|| {
            anyhow!(
                "游戏版本未知，当前插件适用于游戏版本 {}",
                version_config.required_game_version
            )
        })?;

    if current_hash != *expected_hash {
        anyhow::bail!(
            "游戏版本不匹配，当前文件版本哈希: {}，当前插件适用于游戏版本 {}（期望哈希: {}）",
            current_hash,
            version_config.required_game_version,
            expected_hash
        );
    }

    println!(
        "✓ Game.gde 哈希校验通过，符合版本 {}",
        version_config.required_game_version
    );
    Ok(())
}

fn create_plugin_version_content(version_config: &VersionConfig) -> Vec<u8> {
    format!(
        "{}\n{}",
        version_config.required_game_version, version_config.plugin_version
    )
    .into_bytes()
}
