use leptos::{prelude::*, text_prop::TextProp};

#[component]
pub fn Picture(
    #[prop(into)] src: TextProp,
    #[prop(into)] alt: String,
    #[prop(into, optional)] sizes: Option<String>,
) -> impl IntoView {
    let src = src.get().as_str().to_string();
    let srcc = src.clone();
    let srcset = Resource::new_blocking(
        move || srcc.clone(),
        |_src_in| async move {
            #[cfg(feature = "ssr")]
            {
                ssr::make_variants(&_src_in).await
            }
            #[cfg(not(feature = "ssr"))]
            {
                Option::<(String, String, (u32, u32))>::None
            }
        },
    );
    let src = StoredValue::new(src);
    let alt = StoredValue::new(alt);
    let sizes = StoredValue::new(sizes);

    view! {
        <Suspense>
            {
                move || Suspend::new(async move {
                    let (srcset, sizes_gen, width, height) = srcset
                        .await
                        .map(|(srcset, sizes, dim)| (Some(srcset), Some(sizes), Some(dim.0), Some(dim.1)))
                        .unwrap_or((None, None, None, None));
                    view! {
                        <img
                            src = src.get_value()
                            alt = alt.get_value()
                            srcset={srcset}
                            sizes={sizes.get_value().or(sizes_gen)}
                            height={height}
                            width={width}
                        />
                    }
                })
            }
        </Suspense>
    }
}

#[cfg(feature = "ssr")]
pub mod ssr {

    #[cfg_attr(debug_assertions, allow(unused_imports))]
    use std::{
        collections::{HashMap, HashSet},
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    #[cfg_attr(debug_assertions, allow(unused_imports))]
    use image::{ImageReader, imageops::FilterType};
    #[cfg_attr(debug_assertions, allow(unused_imports))]
    use leptos::{config::LeptosOptions, prelude::expect_context};
    #[cfg_attr(debug_assertions, allow(unused_imports))]
    use rayon::prelude::*;
    #[cfg_attr(debug_assertions, allow(unused_imports))]
    use sha2::{Digest, Sha256};
    #[cfg_attr(debug_assertions, allow(unused_imports))]
    use tokio::io::{AsyncReadExt, BufReader};

    #[cfg_attr(debug_assertions, allow(dead_code))]
    #[derive(Clone)]
    pub struct VariantLock {
        paths: Arc<Mutex<HashMap<PathBuf, (u32, u32, HashSet<(u32, PathBuf)>)>>>,
        generation_lock: Arc<tokio::sync::Mutex<()>>,
        pub cache_folder_path: PathBuf,
    }

    #[cfg_attr(debug_assertions, allow(dead_code))]
    impl VariantLock {
        pub fn new(cache_folder: PathBuf) -> VariantLock {
            Self {
                paths: Arc::new(Mutex::new(HashMap::new())),
                cache_folder_path: cache_folder,
                generation_lock: Arc::new(tokio::sync::Mutex::new(())),
            }
        }
    }

    pub async fn make_variants(url: &str) -> Option<(String, String, (u32, u32))> {
        #[cfg(debug_assertions)]
        {
            let _ = url;
            return None;
        }
        #[cfg(not(debug_assertions))]
        {
            println!("Make variants for {url}");
            let mut avif_sizes = vec![];

            let options = expect_context::<LeptosOptions>();
            let variantlock = expect_context::<VariantLock>();
            println!("Locking generation for {url}");
            let generation_lock = variantlock.generation_lock.lock().await;
            println!("Got lock for generation for {url}");

            let path = PathBuf::from(options.site_root.as_ref()).join(url.strip_prefix("/")?);
            let name = if let Some(extension) = path.extension() {
                path.file_name()?
                    .to_str()?
                    .strip_suffix(&format!(".{}", extension.to_str()?))?
            } else {
                path.file_name()?.to_str()?
            };
            let dir = path.parent()?;
            println!("Generate hash");
            let original_path = path.clone();

            let mut width = 0;
            let mut height = 0;
            let mut needed_gen = true;
            {
                if let Ok(mut variants) = variantlock.paths.lock() {
                    if let Some((image_width, image_height, variants_gen)) =
                        variants.get_mut(&original_path)
                    {
                        width = *image_width;
                        height = *image_height;
                        for (size, path) in variants_gen.iter() {
                            avif_sizes.push((*size, path.clone()));
                        }
                        needed_gen = false;
                    }
                }
            }
            if needed_gen {
                let image_hash_result = generate_file_hash(&path).await;

                let img_hash = match image_hash_result {
                    Ok(hash) => hash,
                    Err(err) => {
                        println!("Error generating file hash: {err}");
                        return None;
                    }
                };
                println!("Got hash {img_hash}");
                let cache_dir = variantlock
                    .cache_folder_path
                    .join(format!("{name}-{img_hash}"));
                tokio::fs::create_dir_all(&cache_dir).await.ok()?;

                let path = path.clone();
                let dir = dir.to_path_buf();
                let name = name.to_string();
                let paths = variantlock.paths.clone();
                let original_path = original_path.clone();
                let cache_dir = cache_dir.clone();

                let (w, h, generated) = tokio::task::spawn_blocking(move || {
                    let image = ImageReader::open(&path).ok()?.decode().ok()?;
                    let width = image.width();
                    let height = image.height();
                    let mut sizes = vec![240, 320, 480, 720, 960, 1080, 1440, 1620, 1920];
                    sizes.retain(|size| size < &width);

                    if width > sizes.last().cloned().unwrap_or_default() {
                        sizes.push(width);
                    }

                    let image = Arc::new(image);

                    let generated = sizes
                        .par_iter()
                        .filter_map(|size| {
                            let (ext, format);
                            (ext, format) = ("avif", image::ImageFormat::Avif);
                            let name = format!("{name}-{size}.{ext}");
                            let path = dir.join(&name);
                            let cache_path = cache_dir.join(&name);

                            {
                                if let Ok(mut variants) = paths.lock() {
                                    if let Some((_, _, variants_gen)) =
                                        variants.get_mut(&original_path)
                                    {
                                        if variants_gen.contains(&(*size, path.clone())) {
                                            return Some((*size, path.clone()));
                                        } else {
                                            variants_gen.insert((*size, path.clone()));
                                        }
                                    } else {
                                        let mut variants_gen = HashSet::new();
                                        variants_gen.insert((*size, path.clone()));
                                        variants.insert(
                                            original_path.clone(),
                                            (width, height, variants_gen),
                                        );
                                    }
                                } else {
                                    return None;
                                }
                            }

                            if cache_path.exists() && std::fs::copy(&cache_path, &path).is_ok() {
                                return Some((*size, path));
                            }

                            let new_h = ((*size as f64) / (width as f64)) * (height as f64);
                            let filter = FilterType::Lanczos3;
                            let new_img = image.resize_exact(*size, new_h as u32, filter);

                            if path.exists() {
                                println!("Skip bcz exists {path:?}");
                                Some((*size, path))
                            } else {
                                println!("writing to New w: {size} h {new_h} {path:?}");
                                if new_img.save_with_format(&path, format).is_ok() {
                                    println!("written to New w: {size} h {new_h} {path:?}");
                                    let _ = std::fs::copy(&path, &cache_path);
                                    Some((*size, path))
                                } else {
                                    None
                                }
                            }
                        })
                        .collect::<Vec<_>>();

                    Some((width, height, generated))
                })
                .await
                .ok()??;

                width = w;
                height = h;
                avif_sizes.extend(generated);
            }

            avif_sizes.sort_by(|a, b| a.0.cmp(&b.0));
            let srcs = avif_sizes
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{} {k}w",
                        v.to_string_lossy()
                            .strip_prefix(options.site_root.as_ref())
                            .expect("expected path to start in root")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");

            let sizes_st = avif_sizes
                .iter()
                .map(|(w, _)| {
                    if w == &avif_sizes.last().map(|(w, _)| *w).unwrap_or_default() {
                        format!("{w}px")
                    } else {
                        format!("(max-width: {w}px) {w}px")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");

            println!("Variants generated for {original_path:?}");
            drop(generation_lock);
            Some((srcs, sizes_st, (width, height)))
        }
    }

    #[cfg_attr(debug_assertions, allow(dead_code))]
    async fn generate_file_hash(file_path: &Path) -> std::io::Result<String> {
        println!("Generating file hash for {file_path:?}");
        println!("Opening file {file_path:?}");
        let file = std::fs::File::open(file_path);
        let file = match file {
            Ok(file) => file,
            Err(err) => {
                println!("Error opening file {file_path:?}: {err}");
                return Err(err);
            }
        };
        println!("Opened file {file_path:?}");
        let file = tokio::fs::File::from_std(file);
        let mut reader = BufReader::new(file);
        let mut hasher = Sha256::new();

        let mut buffer = [0; 1024 * 1024]; // 1MB buffer
        loop {
            let count = reader.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }

        let hash = hasher.finalize();
        Ok(hex::encode(hash))
    }
}
