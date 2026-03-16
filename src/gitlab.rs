use anyhow::{bail, Result};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::io::Read;

#[derive(Debug)]
pub struct GitlabClient {
    url: String,
    token: SecretString,
}

#[derive(Debug, Deserialize)]
pub struct Project {
    pub id: u64,
    pub path: String,
    pub path_with_namespace: String,
}

#[derive(Debug, Deserialize)]
pub struct Branch {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct TreeItem {
    pub name: String,
    #[serde(rename = "type")]
    pub item_type: String, // "tree" (dir) or "blob" (file)
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct FileInfo {
    pub size: u64,
    // Note: GitLab API often requires base64 decoding for content, but we will use the /raw endpoint for binary stability.
}

impl GitlabClient {
    pub fn new(url: String, token: String) -> Result<Self> {
        if !url.starts_with("https://") {
            bail!("GitLab URL must use HTTPS to protect the token");
        }
        Ok(Self { url, token: SecretString::from(token) })
    }

    fn request(&self, path: &str) -> ureq::Request {
        let full_url = format!("{}/api/v4{}", self.url, path);
        ureq::get(&full_url)
            .set("PRIVATE-TOKEN", self.token.expose_secret())
    }

    pub fn fetch_projects(&self) -> Result<Vec<Project>> {
        let mut all_projects = Vec::new();
        let mut page = 1;
        loop {
            let endpoint = format!("/projects?membership=true&per_page=100&page={}", page);
            let resp = self.request(&endpoint).call()?;
            let has_next = resp.header("X-Next-Page").is_some_and(|s| !s.is_empty());
            let mut items: Vec<Project> = resp.into_json()?;
            if items.is_empty() { break; }
            all_projects.append(&mut items);
            if !has_next { break; }
            page += 1;
        }
        Ok(all_projects)
    }

    pub fn fetch_branches(&self, project_id: u64) -> Result<Vec<Branch>> {
        let mut all_branches = Vec::new();
        let mut page = 1;
        loop {
            let endpoint = format!("/projects/{}/repository/branches?per_page=100&page={}", project_id, page);
            let resp = self.request(&endpoint).call()?;
            let has_next = resp.header("X-Next-Page").is_some_and(|s| !s.is_empty());
            let mut items: Vec<Branch> = resp.into_json()?;
            if items.is_empty() { break; }
            all_branches.append(&mut items);
            if !has_next { break; }
            page += 1;
        }
        Ok(all_branches)
    }

    pub fn fetch_tree(&self, project_id: u64, path: &str, branch: &str) -> Result<Vec<TreeItem>> {
        let encoded_path = urlencoding::encode(path);
        let encoded_branch = urlencoding::encode(branch);
        let mut all_items = Vec::new();
        let mut page = 1;
        loop {
            let endpoint = format!(
                "/projects/{}/repository/tree?path={}&ref={}&per_page=100&page={}",
                project_id, encoded_path, encoded_branch, page
            );
            let resp = self.request(&endpoint).call()?;
            let has_next = resp.header("X-Next-Page").is_some_and(|s| !s.is_empty());
            let mut items: Vec<TreeItem> = resp.into_json()?;
            if items.is_empty() { break; }
            all_items.append(&mut items);
            if !has_next { break; }
            page += 1;
        }
        Ok(all_items)
    }

    pub fn get_file_info(&self, project_id: u64, file_path: &str, branch: &str) -> Result<FileInfo> {
        let encoded_path = urlencoding::encode(file_path);
        let encoded_branch = urlencoding::encode(branch);
        let endpoint = format!(
            "/projects/{}/repository/files/{}?ref={}",
            project_id, encoded_path, encoded_branch
        );
        let resp = self.request(&endpoint).call()?;
        let info: FileInfo = resp.into_json()?;
        Ok(info)
    }

    pub fn download_file(&self, project_id: u64, file_path: &str, branch: &str) -> Result<Vec<u8>> {
        let encoded_path = urlencoding::encode(file_path);
        let encoded_branch = urlencoding::encode(branch);
        let endpoint = format!(
            "/projects/{}/repository/files/{}/raw?ref={}",
            project_id, encoded_path, encoded_branch
        );
        let resp = self.request(&endpoint).call()?;
        let mut bytes = Vec::new();
        resp.into_reader().take(32 * 1024 * 1024).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}
