use std::sync::{Arc, Mutex};

use crate::SessionStore;
use crate::store_conformance_test::{lifecycle, stores};
use crate::store_coordinator::{AdmittedStore, admission};

#[tokio::test]
async fn admission_decorator_forwards_the_complete_store_contract() {
    // Arrange
    for store in stores().await {
        let admission = admission(store.identity(), "session").expect("admission");
        let decorated: Arc<dyn SessionStore> = Arc::new(AdmittedStore {
            admission: Mutex::new(Some(Arc::new(admission))),
            lease: Mutex::new(None),
            settlement: None,
            store,
        });

        // Act / Assert
        lifecycle(decorated).await;
    }
}
