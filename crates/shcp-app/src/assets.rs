//! Serves the embedded icons to gpui's svg element.
use crate::widgets::icon::IconName;
use anyhow::Result;
use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(IconName::ALL
            .iter()
            .find(|icon| icon.path() == path)
            .map(|icon| Cow::Borrowed(icon.bytes())))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(IconName::ALL
            .iter()
            .filter(|icon| icon.path().starts_with(path))
            .map(|icon| SharedString::from(icon.path()))
            .collect())
    }
}
