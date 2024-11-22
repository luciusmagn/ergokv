use ergokv::{LocalCluster, Store};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use uuid::Uuid;

#[derive(
    Store, Serialize, Deserialize, Debug, PartialEq, Clone,
)]
struct User {
    #[key]
    id: Uuid,
    #[unique_index]
    username: String,
    #[index]
    email: String,
    #[index]
    department: String,
}

#[tokio::test]
async fn test_backup_restore() {
    let tmp = TempDir::new().expect("Failed to create temp dir");
    let tikv_instance = LocalCluster::start(tmp.path()).unwrap();
    let client = tikv_instance.spawn_client().await.unwrap();

    // Create test users
    let users = vec![
        User {
            id: Uuid::new_v4(),
            username: "alice".to_string(),
            email: "alice@example.com".to_string(),
            department: "Engineering".to_string(),
        },
        User {
            id: Uuid::new_v4(),
            username: "bob".to_string(),
            email: "bob@example.com".to_string(),
            department: "Engineering".to_string(),
        },
    ];

    // Save users
    let mut txn = client.begin_optimistic().await.unwrap();
    for user in &users {
        user.save(&mut txn).await.unwrap();
    }
    txn.commit().await.unwrap();

    // Verify index methods work
    let mut txn = client.begin_optimistic().await.unwrap();
    let alice_by_username = User::by_username("alice", &mut txn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice_by_username, users[0]);

    let users_in_engineering =
        User::by_department("Engineering", &mut txn)
            .await
            .unwrap();
    assert_eq!(users_in_engineering.len(), 2);
    txn.commit().await.unwrap();

    // Create backup directory
    let backup_dir = tmp.path().join("backups");
    std::fs::create_dir(&backup_dir).unwrap();

    // Backup
    let mut txn = client.begin_optimistic().await.unwrap();
    let backup_path =
        User::backup(&mut txn, &backup_dir).await.unwrap();
    txn.commit().await.unwrap();
    assert!(backup_path.exists());

    // Delete all users
    let mut txn = client.begin_optimistic().await.unwrap();
    for user in &users {
        user.delete(&mut txn).await.unwrap();
    }
    txn.commit().await.unwrap();

    // Verify users are gone
    let mut txn = client.begin_optimistic().await.unwrap();
    let mut found = Vec::new();
    {
        let stream = User::all(&mut txn);
        futures::pin_mut!(stream);
        while let Some(Ok(user)) = stream.next().await {
            found.push(user);
        }
    }
    assert!(found.is_empty());
    txn.commit().await.unwrap();

    // Restore from backup
    let mut txn = client.begin_optimistic().await.unwrap();
    User::restore(&mut txn, backup_path).await.unwrap();
    txn.commit().await.unwrap();

    // Verify restored data
    let mut txn = client.begin_optimistic().await.unwrap();
    let mut restored = Vec::new();
    {
        let stream = User::all(&mut txn);
        futures::pin_mut!(stream);
        while let Some(Ok(user)) = stream.next().await {
            restored.push(user);
        }
    }

    // Sort both vectors for comparison
    let mut users = users.clone();
    users.sort_by(|a, b| a.username.cmp(&b.username));
    restored.sort_by(|a, b| a.username.cmp(&b.username));

    assert_eq!(users, restored);
    txn.commit().await.unwrap();
}
