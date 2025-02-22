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
        |src| async move {
            #[cfg(feature = "ssr")]
            {
                ssr::make_variants(&src).await
            }
            #[cfg(not(feature = "ssr"))]
            {
                Option::<(String, String, (u32, u32))>::None
            }
        },
    );
    Suspend::new(async move {
        let (srcset, sizes_gen, width, height) = srcset
            .await
            .map(|(srcset, sizes, dim)| (Some(srcset), Some(sizes), Some(dim.0), Some(dim.1)))
            .unwrap_or((None, None, None, None));
        view! {
            <img
                src={src}
                srcset={srcset}
                sizes={sizes.or(sizes_gen)}
                height={height}
                width={width}
                alt={alt}
            />
        }
    })
}

#[cfg(feature = "ssr")]
pub mod ssr {

    use std::{
        collections::HashSet,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    use image::{imageops::FilterType::Lanczos3, ImageReader};
    use leptos::{config::LeptosOptions, prelude::expect_context};
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, BufReader};

    #[derive(Clone)]
    pub struct VariantLock {
        paths: Arc<Mutex<HashSet<PathBuf>>>,
        pub cache_folder_path: PathBuf,
    }

    impl VariantLock {
        pub fn new(cache_folder: PathBuf) -> VariantLock {
            Self {
                paths: Arc::new(Mutex::new(HashSet::new())),
                cache_folder_path: cache_folder,
            }
        }
    }

    pub async fn make_variants(url: &str) -> Option<(String, String, (u32, u32))> {
        println!("Make variants for {url}");
        let options = expect_context::<LeptosOptions>();
        let variantlock = expect_context::<VariantLock>();
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
        let img_hash = generate_file_hash(&path).await.ok()?;
        println!("Got hash {img_hash}");
        let cache_dir = variantlock
            .cache_folder_path
            .join(format!("{name}-{img_hash}"));
        tokio::fs::create_dir_all(&cache_dir).await.ok()?;

        let image = ImageReader::open(&path).ok()?.decode().ok()?;
        let width = image.width();
        let height = image.height();
        let mut sizes = vec![240, 320, 480, 720, 960, 1080, 1440, 1620, 1920];
        sizes.retain(|size| size < &width);

        if width > sizes.last().cloned().unwrap_or_default() {
            sizes.push(width);
        }

        let mut avif_sizes = vec![];
        for size in sizes.iter() {
            let (ext, format);
            #[cfg(debug_assertions)]
            {
                (ext, format) = ("png", image::ImageFormat::Png);
            }
            #[cfg(not(debug_assertions))]
            {
                (ext, format) = ("avif", image::ImageFormat::Avif);
            }
            let name = format!("{name}-{size}.{ext}");
            let path = dir.join(&name);
            let cache_path = cache_dir.join(&name);

            if let Ok(mut variants) = variantlock.paths.lock() {
                if variants.contains(&path) {
                    avif_sizes.push((*size, path));
                    continue;
                } else {
                    variants.insert(path.clone());
                }
            } else {
                continue;
            }
            if Some(true) == tokio::fs::try_exists(&cache_path).await.ok()
                && tokio::fs::copy(&cache_path, &path).await.is_ok()
            {
                avif_sizes.push((*size, path));
                continue;
            }

            let img = image.clone();

            let new_h = ((*size as f64) / (width as f64)) * (height as f64);
            let new_img = img.resize_exact(*size, new_h as u32, Lanczos3);

            if let Ok(exists) = tokio::fs::try_exists(&path).await {
                let p2 = path.clone();
                if exists {
                    println!("Skip bcz exists {path:?}");
                    avif_sizes.push((*size, path));
                } else if let Ok(data) = tokio::task::spawn_blocking(move || async move {
                    new_img.save_with_format(&path, format)
                })
                .await
                {
                    println!("writing to New w: {size} h {new_h} {p2:?}");
                    if data.await.is_ok() {
                        println!("written to New w: {size} h {new_h} {p2:?}");
                        let _ = tokio::fs::copy(&p2, &cache_path).await;
                        avif_sizes.push((*size, p2));
                    }
                }
            } else {
                println!("Skip bcz error {path:?}");
            }
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

        Some((srcs, sizes_st, (width, height)))
    }

    async fn generate_file_hash(file_path: &Path) -> std::io::Result<String> {
        let file = tokio::fs::File::open(file_path).await?;
        let mut reader = BufReader::new(file);
        let mut hasher = Sha256::new();

        let mut buffer = [0; 1024]; // 1MB buffer
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