//! Project filesystem: lazily-loaded file tree, background file index, filesystem
//! watcher and project-wide text search.

pub mod index;
pub mod search;
pub mod tree;
pub mod watcher;

pub use index::FileIndex;
pub use search::{ProjectSearch, SearchEvent, SearchQuery, SearchResult};
pub use tree::FileTree;
pub use watcher::FsWatcher;
