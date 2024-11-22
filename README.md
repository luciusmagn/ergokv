# ergokv

`DISCLAIMER:` THIS IS ALPHA AS FUCK. (not yet suitable for production)

A Rust library for easy integration with TiKV, providing derive macros
for automatic CRUD operations.

[![](https://img.shields.io/crates/v/ergokv.svg)](https://crates.io/crates/ergokv)
[![](https://docs.rs/ergokv/badge.svg)](https://docs.rs/ergokv)
[![](https://img.shields.io/badge/license-Fair-blue.svg)](https://github.com/luciumagn/ergokv/blob/main/LICENSE)

## Installation

Add this to your `Cargo.toml`:

``` toml
[dependencies]
ergokv = "0.1.8"
```

## Documentation

For detailed documentation, including usage examples and API reference,
please visit:

[<https://docs.rs/ergokv>](https://docs.rs/ergokv)

You can also generate the documentation locally by running:

``` bash
cargo doc --open
```

## Prerequisites

- Rust (edition 2021 or later)
- Protobuf
- GRPC
- TiKV (can be installed via TiUP or automatically via LocalCluster)

### Installing TiKV

There are two primary ways to install TiKV:

1.  Manual Installation with TiUP:

    ``` bash
    # Install TiUP (TiKV's package manager)
    curl --proto '=https' --tlsv1.2 -sSf https://tiup-mirrors.pingcap.com/install.sh | sh

    # Set up TiKV cluster for development
    tiup playground
    ```

2.  Automatic Installation with LocalCluster:

    ``` rust
    use ergokv::LocalCluster;

    // LocalCluster automatically downloads and sets up TiKV if not present
    let cluster = LocalCluster::start(temp_dir).unwrap();
    let client = cluster.spawn_client().await.unwrap();
    ```

    LocalCluster is particularly useful for development and testing, as
    it automatically handles TiKV installation and cluster setup.

## Attributes

The `Store` derive supports several attributes to customize your data
model:

- `@[key]`: Marks the primary key field (exactly one field must have
  this attribute)

  ``` rust
  #[derive(Store)]
  struct User {
      #[key]
      id: Uuid,  // Primary key for identifying the entity
  }
  ```

- `@[unique_index]`: Creates a unique index on a field, allowing
  efficient lookup with guaranteed uniqueness

  ``` rust
  #[derive(Store)]
  struct User {
      #[key]
      id: Uuid,
      #[unique_index]
      username: String,  // 1:1 mapping
  }
  ```

- `@[index]`: Creates a non-unique index on a field, allowing multiple
  entities to share the same indexed value

  ``` rust
  #[derive(Store)]
  struct User {
      #[key]
      id: Uuid,
      #[index]
      department: String,  // Multiple users can be in the same department
  }
  ```

- `@[migrate_from]`: Used for schema migrations, specifying the previous
  version of the struct

  ``` rust
  #[derive(Store)]
  #[migrate_from(OldUser)]
  struct User {
      // Migration logic implementation
  }
  ```

- `@[model_name]`: Used during migrations when the struct name changes

  ``` rust
  #[derive(Store)]
  #[model_name = "User"]  // Helps track model across versions
  struct UserV2 {
      // Struct definition
  }
  ```

## Usage

Basic usage with various index types:

``` rust
use ergokv::Store;
use serde::{Serialize, Deserialize};
use uuid::Uuid;

#[derive(Store, Serialize, Deserialize)]
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use LocalCluster for easy development setup
    let cluster = LocalCluster::start(std::env::temp_dir()).await?;
    let client = cluster.spawn_client().await?;

    // Create a new user
    let user = User {
        id: Uuid::new_v4(),
        username: "johndoe",
        email: "john@example.com",
        department: "Engineering",
    };

    let mut txn = client.begin_optimistic().await?;

    // Save the user
    user.save(&mut txn).await?;
    txn.commit().await?;

    // Lookup methods
    let mut txn = client.begin_optimistic().await?;
    let user_by_username = User::by_username("johndoe", &mut txn).await?;
    let users_in_engineering = User::by_department("Engineering", &mut txn).await?;

    Ok(())
}
```

Longer example:

``` rust
use ergokv::Store;
use serde::{Serialize, Deserialize};
use uuid::Uuid;

#[derive(Store, Serialize, Deserialize)]
struct User {
    #[key]
    id: Uuid,
    #[unique_index]
    username: String,
    email: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Set up TiKV client
    let client = tikv_client::TransactionClient::new(vec!["127.0.0.1:2379"]).await?;

    // Create a new user
    let user = User {
        id: Uuid::new_v4(),
        username: "testuser".to_string(),
        email: "test@example.com".to_string(),
    };

    // Start a transaction
    let mut txn = client.begin_optimistic().await?;

    // Save the user
    user.save(&mut txn).await?;

    // Commit the transaction
    txn.commit().await?;

    // Load the user
    let mut txn = client.begin_optimistic().await?;
    let loaded_user = User::load(&user.id, &mut txn).await?;
    println!("Loaded user: {:?}", loaded_user);

    Ok(())
}
```

## Backup and Restore

The `Store` derive automatically implements backup and restore
functionality for your models:

``` rust
use ergokv::Store;
use serde::{Serialize, Deserialize};
use uuid::Uuid;

#[derive(Store, Serialize, Deserialize)]
struct User {
    #[key]
    id: Uuid,
    #[unique_index]
    username: String,
    email: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = tikv_client::TransactionClient::new(vec!["127.0.0.1:2379"]).await?;

    // Backup all users
    let mut txn = client.begin_optimistic().await?;
    let backup_path = User::backup(&mut txn, "backups/").await?;
    println!("Backup created at: {}", backup_path.display());
    txn.commit().await?;

    // Restore from backup
    let mut txn = client.begin_optimistic().await?;
    User::restore(&mut txn, backup_path).await?;
    txn.commit().await?;

    Ok(())
}
```

Backups are stored as line-delimited JSON files, with automatic
timestamping: `User_1708644444.json`. Each line contains one serialized
instance, making the backups human-readable and easy to process with
standard tools.

## Migrations

Store migrations are supported via the \`#\[migrate<sub>from</sub>\]\`
attribute. This allows you to evolve your data structures while keeping
data integrity.

### Example

The recommended approach is to use private submodules for versioning
models and always re-export the latest version:

``` rust
mod models {
    mod v1 {
        #[derive(Store, Serialize, Deserialize)]
        #[model_name = "User"]  // Required when struct was renamed
        pub(super) struct UserV1 {
            #[key]
            id: Uuid,
            name: String,
            email: String,
        }
    }

    mod v2 {
        #[derive(Store, Serialize, Deserialize)]
        #[migrate_from(super::v1::UserV1)]
        pub(super) struct User {
            #[key]
            id: Uuid,
            first_name: String,
            last_name: String,
            email: String,
        }

        impl UserV1ToUser for User {
            fn from_user_v1(prev: &super::v1::UserV1) -> Result<Self, tikv_client::Error> {
                let (first, last) = prev.name
                    .split_once(' ')
                    .ok_or_else(|| tikv_client::Error::StringError(
                        "Invalid name format".into()
                    ))?;

                Ok(Self {
                    id: prev.id,
                    first_name: first.to_string(),
                    last_name: last.to_string(),
                    email: prev.email.clone(),
                })
            }
        }
    }

    // Always re-export latest version
    pub use v2::User;
}
```

Note: The \`#\[model<sub>name</sub>\]\` attribute is required when the
struct name changes between versions (like UserV1 -\> User above). This
ensures ergokv can track the underlying model correctly across
migrations.

Run migrations:

``` rust
User::ensure_migrations(&client).await?;
```

## Running TiKV

### For Development

Use TiUP playground:

``` bash
tiup playground
```

This sets up a local TiKV cluster for testing.

### For Production

1.  Create a topology file (e.g., \`topology.yaml\`):

    ``` yaml
    global:
      user: "tidb"
      ssh_port: 22
      deploy_dir: "/tidb-deploy"
      data_dir: "/tidb-data"

    pd_servers:
      - host: 10.0.1.1
      - host: 10.0.1.2
      - host: 10.0.1.3

    tikv_servers:
      - host: 10.0.1.4
      - host: 10.0.1.5
      - host: 10.0.1.6

    tidb_servers:
      - host: 10.0.1.7
      - host: 10.0.1.8
      - host: 10.0.1.9
    ```

2.  Deploy the cluster:

    ``` bash
    tiup cluster deploy mytikvcluster 5.1.0 topology.yaml --user root -p
    ```

3.  Start the cluster:

    ``` bash
    tiup cluster start mytikvcluster
    ```

## Testing

To run tests, ensure you have TiUP installed and then use:

``` bash
cargo test
```

Tests will automatically start and stop a TiKV instance using TiUP.

I will be honest with you, chief, I made one test and that's it.

## License

This project is licensed under the Fair License:

> Copyright (c) 2024 Lukáš Hozda
>
> Usage of the works is permitted provided that this instrument is
> retained with the works, so that any entity that uses the works is
> notified of this instrument.
>
> DISCLAIMER: THE WORKS ARE WITHOUT WARRANTY.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

There is a lot of things that could be improved:

- Make ergokv support more KV stores
- Improve documentation
- Allow swapping the serialization format (currently we use CBOR via
  ciborium)
- Let methods be generic (in the case of TiKV) over RawClient,
  Transaction and TransactionClient
- Add additional methods that retrieve multiple structures, to make it
  useful to e.g. fetch entities like articles and all users (note that
  this can be done already by manually making a sort of entity registry
  for yourself)

## GitHub Repository

[github.com/luciumagn/ergokv](https://github.com/luciumagn/ergokv)
