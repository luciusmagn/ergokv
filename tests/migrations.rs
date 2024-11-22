use ergokv::LocalCluster;
use tempfile::TempDir;
use uuid::Uuid;

mod version1 {
    use super::*;
    use ergokv::Store;
    use serde::{Deserialize, Serialize};

    #[derive(
        Store, Serialize, Deserialize, Debug, PartialEq,
    )]
    #[model_name = "User"]
    pub struct User {
        #[key]
        pub id: Uuid,
        #[unique_index]
        pub name: String,
        pub email: String,
    }
}

mod version2 {
    use super::*;
    use ergokv::Store;
    use serde::{Deserialize, Serialize};

    #[derive(
        Store, Serialize, Deserialize, Debug, PartialEq,
    )]
    #[migrate_from(version1::User)]
    pub struct User {
        #[key]
        pub id: Uuid,
        #[unique_index]
        pub first_name: String,
        pub last_name: String,
        pub email: String,
    }

    impl UserToUser for User {
        fn from_user(
            prev: &super::version1::User,
        ) -> Result<Self, tikv_client::Error> {
            let (first, last) =
                prev.name.split_once(' ').ok_or_else(|| {
                    tikv_client::Error::StringError(
                        "Invalid name format".into(),
                    )
                })?;

            Ok(Self {
                id: prev.id,
                first_name: first.to_string(),
                last_name: last.to_string(),
                email: prev.email.clone(),
            })
        }
    }
}

// Migration test
#[tokio::test]
async fn test_migrations() {
    let tmp = TempDir::new().expect("Failed to create temp dir");
    let tikv_instance = LocalCluster::start(tmp.path()).unwrap();
    let client = tikv_instance.spawn_client().await.unwrap();

    pub use version2::User;

    // Create and store old version
    let user_v1 = version1::User {
        id: Uuid::new_v4(),
        name: "John Doe".into(),
        email: "john@example.com".into(),
    };

    let mut txn = client.begin_optimistic().await.unwrap();
    user_v1.save(&mut txn).await.unwrap();
    txn.commit().await.unwrap();

    // Run migrations
    User::ensure_migrations(&client).await.unwrap();

    // Verify migrated data
    let mut txn = client.begin_optimistic().await.unwrap();
    let migrated =
        User::load(&user_v1.id, &mut txn).await.unwrap();
    txn.commit().await.unwrap();

    assert_eq!(migrated.id, user_v1.id);
    assert_eq!(migrated.first_name, "John");
    assert_eq!(migrated.last_name, "Doe");
    assert_eq!(migrated.email, user_v1.email);
}
