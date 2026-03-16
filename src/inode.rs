use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FsNode {
    Root,                                                 // /
    Projects,                                             // /projects
    Namespace { name: String },                           // /projects/{namespace}
    Project { namespace: String, name: String, id: u64 }, // /projects/{namespace}/{project}
    BranchDir { project_id: u64, branch: String }, // /projects/.../{branch}
    GitDir { project_id: u64, branch: String, path: String }, // /projects/.../{branch}/some/path
    GitFile { project_id: u64, branch: String, path: String }, // /projects/.../{branch}/some/file.txt
}

pub struct InodeTracker {
    next_ino: u64,
    ino_to_node: HashMap<u64, FsNode>,
    node_to_ino: HashMap<FsNode, u64>,
    lookup_counts: HashMap<u64, u64>,
}

impl Default for InodeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl InodeTracker {
    pub fn new() -> Self {
        let mut tracker = Self {
            next_ino: 2, // 1 is root
            ino_to_node: HashMap::new(),
            node_to_ino: HashMap::new(),
            lookup_counts: HashMap::new(),
        };
        // Insert Root at inode 1
        tracker.ino_to_node.insert(1, FsNode::Root);
        tracker.node_to_ino.insert(FsNode::Root, 1);
        tracker
    }

    pub fn get_node(&self, ino: u64) -> Option<&FsNode> {
        self.ino_to_node.get(&ino)
    }

    pub fn insert_or_get(&mut self, node: FsNode) -> u64 {
        if let Some(&ino) = self.node_to_ino.get(&node) {
            ino
        } else {
            let ino = self.next_ino;
            self.next_ino += 1;
            self.ino_to_node.insert(ino, node.clone());
            self.node_to_ino.insert(node, ino);
            ino
        }
    }

    pub fn inc_lookup(&mut self, ino: u64) {
        if ino == 1 { return; }
        *self.lookup_counts.entry(ino).or_insert(0) += 1;
    }

    pub fn forget(&mut self, ino: u64, nlookup: u64) {
        if ino == 1 { return; }
        if let Some(count) = self.lookup_counts.get_mut(&ino) {
            *count = count.saturating_sub(nlookup);
            if *count == 0 {
                self.lookup_counts.remove(&ino);
                if let Some(node) = self.ino_to_node.remove(&ino) {
                    self.node_to_ino.remove(&node);
                }
            }
        }
    }
}
