use anyhow::{Context, Result};
use serde::Deserialize;
use std::io::Read;

#[derive(Debug, Clone)]
pub struct GitlabClient {
    pub url: String,
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct Project {
    pub id: u64,
    pub name: String,
    pub path: String,
    pub path_with_namespace: String,
    pub default_branch: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Branch {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct TreeItem {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub item_type: String, // "tree" (dir) or "blob" (file)
    pub path: String,
    pub mode: String,
}

#[derive(Debug, Deserialize)]
pub struct FileInfo {
    pub file_name: String,
    pub file_path: String,
    pub size: u64,
    pub encoding: String,
    pub commit_id: String,
    // Note: GitLab API often requires base64 decoding for content, but we will use the /raw endpoint for binary stability.
}

impl GitlabClient {
    pub fn new(url: String, token: String) -> Self {
        Self { url, token }
    }

    fn request(&self, path: &str) -> ureq::Request {
        let full_url = format!("{}/api/v4{}", self.url, path);
        ureq::get(&full_url)
            .set("PRIVATE-TOKEN", &self.token)
    }

    pub fn fetch_projects(&self) -> Result<Vec<Project>> {
        let resp = self.request("/projects?membership=true&per_page=100").call()?;
        let projects: Vec<Project> = resp.into_json()?;
        Ok(projects)
    }

    pub fn fetch_branches(&self, project_id: u64) -> Result<Vec<Branch>> {
        let endpoint = format!("/projects/{}/repository/branches?per_page=100", project_id);
        let resp = self.request(&endpoint).call()?;
        let branches: Vec<Branch> = resp.into_json()?;
        Ok(branches)
    }

    pub fn fetch_tree(&self, project_id: u64, path: &str, branch: &str) -> Result<Vec<TreeItem>> {
        let encoded_path = urlencoding::encode(path);
        let encoded_branch = urlencoding::encode(branch);
        let endpoint = format!(
            "/projects/{}/repository/tree?path={}&ref={}&per_page=100",
            project_id, encoded_path, encoded_branch
        );
        let resp = self.request(&endpoint).call()?;
        let items: Vec<TreeItem> = resp.into_json()?;
        Ok(items)
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
        resp.into_reader().read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}
