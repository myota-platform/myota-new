// MyOTA — C4 model in Structurizr DSL
//
// This is the authoritative model of the system: people, the software system,
// its containers, external systems, and the relationships between them. The
// Mermaid diagrams in the sibling documents are a readable projection of this
// model. When the two disagree, this file is right and the diagram is stale.
//
// Render (requires Java 17+, or the Docker image):
//
//   structurizr-cli export -workspace workspace.dsl -format mermaid -output out
//   structurizr-cli export -workspace workspace.dsl -format plantuml -output out
//   docker run --rm -v "$PWD":/usr/local/structurizr structurizr/cli \
//     export -workspace workspace.dsl -format mermaid -output /usr/local/structurizr/out
//
// Or paste the contents into https://structurizr.com/dsl — no install needed.
//
// Validated with structurizr-cli 2025.11.09: `validate` passes and `export
// -format mermaid` produces all six views. Check it with:
//
//   python3 tools/check_diagrams.py --full
//
// Grammar notes that are easy to get wrong (each cost a debugging round):
//   - tags are ONE comma-separated string, never separate arguments
//   - softwareSystem takes name, description, tags — there is no technology field
//   - the DSL is line-oriented: "{" must end the line
//   - !identifiers hierarchical would make containers resolvable only as
//     myota.<name> from outside the software system, so it is deliberately absent
//   - a deployment view's environment name doubles as its key ([a-zA-Z0-9_-])
//   - "theme default" fetches over the network and is omitted so this validates offline

workspace "MyOTA" "Programme-agnostic outdoor activation platform for amateur radio initiatives." {

    // Flat identifiers: every element name below is unique, so relationships can use
    // short names at any scope. With `!identifiers hierarchical` a container declared
    // inside the software system would only resolve as `myota.<name>` from outside it.

    model {

        // ------------------------------------------------------------------
        // People
        // ------------------------------------------------------------------

        participant = person "Participant" "Finds eligible places, activates them, logs QSOs, and earns awards. Uses the participant login."
        administrator = person "Administrator" "Curates the place catalogue, defines programmes and policy, and runs award issuance. Uses the admin login with a second factor."

        // ------------------------------------------------------------------
        // The system
        // ------------------------------------------------------------------

        myota = softwareSystem "MyOTA" "Activation platform: spatial catalogue, activations, awards, and programme administration." {

            gateway = container "API Gateway" "Single entry point. Holds the composed OpenAPI contract, serves the playground at /docs, routes by prefix, and enforces the session audience against the route class. Stateless; no database." "Go 1.24, Chi"

            core = container "core" "Identity, credentials, sessions and MFA, callsigns, roles, programme configuration and content, and the single notification ledger. Runs as api, worker, and relay from one image." "Rust 2024, Axum, SQLx"
            geodata = container "geodata" "Spatial catalogue: entities and geometry, source adapters and imports, conflation, review, and provenance. PostGIS lives only here." "Rust 2024, Axum, SQLx, geozero"
            activity = container "activity" "Activations, QSOs and ADIF import, award definitions and issuance, statistics and leaderboards." "Go 1.24, Echo, pgx"

            adminWeb = container "Admin Console" "Operator interface: sign-in with a second factor, imports and review, programmes and awards, identity administration. Holds no token in storage: the refresh credential is an HttpOnly cookie and the access token lives in memory." "Vue 3, Vite" "Static"
            participantWeb = container "Participant App" "Participant interface: discover, map, live activation, awards." "Svelte 5, Vite" "Static"

            coreDb = container "myota_core" "Accounts, sessions, roles, programmes, notifications, and the ops schema." "PostgreSQL 16" "Database"
            geoDb = container "myota_geo" "Entities, geometry, sources, imports, conflation, provenance, and the ops schema." "PostgreSQL 16, PostGIS" "Database"
            activityDb = container "myota_activity" "Activations, QSOs, awards, statistics, and the ops schema." "PostgreSQL 16" "Database"
        }

        // ------------------------------------------------------------------
        // External systems
        // ------------------------------------------------------------------

        objectStorage = softwareSystem "Silo object storage" "S3-compatible store (PGSTY Silo) in namespace silo, shared with other tenants. Three buckets per environment: imports (expires 30 days), content, records. Uploads use presigned URLs signed for the public host, so file bytes never traverse an API replica." "External"
        smtp = softwareSystem "SMTP Relay" "SMTP relay for outbound email. Consumed by core only, through the notification ledger." "External"

        // ------------------------------------------------------------------
        // Relationships — people to front ends
        // ------------------------------------------------------------------

        participant -> participantWeb "Browses places, activates, and tracks awards" "HTTPS"
        administrator -> adminWeb "Curates the catalogue and administers programmes" "HTTPS"

        // ------------------------------------------------------------------
        // Relationships — front ends to gateway
        // ------------------------------------------------------------------

        participantWeb -> gateway "Calls the participant API" "HTTPS, JSON"
        adminWeb -> gateway "Calls the admin API" "HTTPS, JSON"

        // ------------------------------------------------------------------
        // Relationships — gateway to services
        // ------------------------------------------------------------------

        gateway -> core "Routes /v1/auth, /v1/me, /v1/programmes, /v1/admin/auth" "HTTPS"
        gateway -> geodata "Routes /v1/entities, /v1/imports, /v1/tiles, /v1/proposals" "HTTPS"
        gateway -> activity "Routes /v1/activations, /v1/qsos, /v1/awards, /v1/statistics" "HTTPS"

        // ------------------------------------------------------------------
        // Relationships — services to their own database
        //
        // One owner per database. Nothing else connects to it. Connections pass
        // through PgBouncer in deployment; that is infrastructure, modelled in
        // the deployment view, not a container of the system.
        // ------------------------------------------------------------------

        core -> coreDb "Reads and writes core state" "SQL"
        geodata -> geoDb "Reads and writes spatial state" "SQL"
        activity -> activityDb "Reads and writes activity state" "SQL"

        // ------------------------------------------------------------------
        // Relationships — services to external systems
        // ------------------------------------------------------------------

        geodata -> objectStorage "Streams import sources and writes attachments" "HTTPS, presigned"
        activity -> objectStorage "Stores award certificates" "HTTPS, presigned"
        core -> objectStorage "Stores callsign evidence" "HTTPS, presigned"
        core -> smtp "Sends verification, recovery, and address-change mail" "SMTP over TLS"

        // ------------------------------------------------------------------
        // Relationships — service to service
        //
        // Events are the only asynchronous path and there is no broker: each
        // service writes to its own ops.outbox_event and peers pull from it.
        // The three synchronous calls are bounded integrity checks, not request
        // composition.
        // ------------------------------------------------------------------

        core -> activity "Publishes account and programme facts" "Events, pulled"
        core -> geodata "Publishes programme and account facts" "Events, pulled"
        geodata -> activity "Publishes entity published and retired facts" "Events, pulled"
        activity -> core "Publishes award issued and activation closed facts" "Events, pulled"

        // The one authorisation decision. Every service asks `core`, with a 60-second
        // service token from the client-credentials exchange; the answer is recomputed
        // from the open grants rather than read from the caller's token.
        geodata -> core "Asks whether an account may review or import (POST /internal/authorize)" "HTTPS, 2s timeout, 60 s service token"
        activity -> core "Asks whether an account may manage awards (POST /internal/authorize)" "HTTPS, 2s timeout, 60 s service token"
        activity -> geodata "Reads entity geometry when an activation closes" "HTTPS, 2s timeout"
        geodata -> activity "Reads deletion impact before retiring an entity" "HTTPS, 2s timeout"

        // ------------------------------------------------------------------
        // Deployment — local K3s, namespace myota, Fleet workspace myota
        // ------------------------------------------------------------------

        deploymentEnvironment "Local K3s" {

            deploymentNode "spainip-k3s" "Single-node K3s cluster" "K3s" {

                deploymentNode "myota" "Kubernetes namespace myota, Fleet workspace myota" "Kubernetes" {

                    infrastructureNode traefik "TLS termination, rate limiting, circuit breaking, retries. Single ingress; counters are global." "Traefik"
                    infrastructureNode pgbouncer "Transaction-mode pooler, one instance per database. Replicas hold pools of 20; the pooler presents 25 server connections regardless of replica count." "PgBouncer"
                    infrastructureNode fleetConfig "Non-secret configuration, reconciled from Git by Fleet. A change is a pull request; a rollback is a revert." "Kubernetes ConfigMap"
                    infrastructureNode sopsSecrets "SOPS-encrypted secrets under deploy/myota/secrets. Applied by an operator step, never reconciled, because an automated sync that can decrypt is an automated sync that can leak." "SOPS + age"
                    infrastructureNode signingSecret "myota-core-signing: the Ed25519 seed that signs access and service tokens, and the AES-256-GCM key that encrypts TOTP seeds at rest. Read only by core; the gateway fetches the public half as a JWKS." "Kubernetes Secret"
                    infrastructureNode serviceClients "myota-core-service-clients: one credential per calling service, exchanged for a 60-second service token. Absent means no service may call an internal endpoint." "Kubernetes Secret"

                    containerInstance gateway
                    containerInstance core
                    containerInstance geodata
                    containerInstance activity
                    containerInstance adminWeb
                    containerInstance participantWeb

                    containerInstance coreDb
                    containerInstance geoDb
                    containerInstance activityDb
                }
            }
        }
    }

    // ----------------------------------------------------------------------
    // Views
    // ----------------------------------------------------------------------

    views {

        systemContext myota "SystemContext" "Who uses MyOTA and what it depends on." {
            include *
            autolayout lr
        }

        container myota "Containers" "Services, front ends, and one database each." {
            include *
            autolayout lr
        }

        container myota "DataOwnership" "Which container owns which database, and nothing else connects." {
            include core
            include geodata
            include activity
            include coreDb
            include geoDb
            include activityDb
            autolayout lr
        }

        // The import pipeline as a numbered flow. Steps run in declaration
        // order and must correspond to relationships declared above.
        dynamic myota "ImportPipeline" "An administrator imports a geodata source and publishes the result." {
            administrator -> adminWeb "Creates an import and uploads parts directly to object storage"
            adminWeb -> gateway "POST /v1/imports, then /complete"
            gateway -> geodata "Creates the run and enqueues ops.job in one transaction"
            geodata -> objectStorage "Streams and normalises the source"
            geodata -> geoDb "Stages candidates, validates, scores conflation"
            administrator -> adminWeb "Reviews candidates and promotes a selection"
            adminWeb -> gateway "POST /v1/imports/{id}/promote"
            gateway -> geodata "Promotes atomically, writes entities and outbox"
            geodata -> geoDb "Commits entities with provenance"
        }

        // Registration and activation, end to end.
        dynamic myota "RegistrationFlow" "A participant registers and activates their account." {
            participant -> participantWeb "Submits the registration form"
            participantWeb -> gateway "POST /v1/auth/register"
            gateway -> core "Creates a pending account and a verification token"
            core -> coreDb "Commits account, token and notification in one transaction"
            core -> smtp "Sends account-verify.v1 with a single-use link"
            participant -> participantWeb "Clicks the link in the mail"
            participantWeb -> gateway "GET /v1/auth/verify?t=..."
            gateway -> core "Redeems the token: purpose and address checked, marked used"
            core -> coreDb "account.status becomes active, session issued"
            core -> smtp "Sends account-welcome.v1"
        }

        // An administrator signs in. The password buys a challenge; only a second
        // factor buys a session, and the gateway refuses the session's audience on any
        // route that did not issue it.
        dynamic myota "AdminSignIn" "An administrator signs in with a password and a TOTP code." {
            administrator -> adminWeb "Enters address and password"
            adminWeb -> gateway "POST /v1/admin/auth/login"
            gateway -> core "Verifies the password, finds a confirmed factor, issues a challenge"
            core -> coreDb "Writes mfa_challenge, the failed-attempt count, the audit row"
            administrator -> adminWeb "Enters the six-digit code"
            adminWeb -> gateway "POST /v1/admin/auth/mfa/challenge"
            gateway -> core "Consumes the challenge and creates the session"
            core -> coreDb "Commits auth_session with audience=admin and the access token id"
            adminWeb -> gateway "Stores no token; the browser holds the __Host-myota_admin_sid cookie"
            administrator -> adminWeb "Lands on the console with an administrator session"
        }

        // One service asking whether an account may do something.
        dynamic myota "AuthorizeDecision" "geodata asks core whether an account may review an entity." {
            geodata -> core "POST /internal/token with its own credential"
            core -> coreDb "Compares the credential and records nothing: the token is stateless"
            geodata -> core "POST /internal/authorize with the account, scope and programme"
            core -> coreDb "Re-reads the open grants and the session"
            core -> geodata "200 {allowed, effectiveScopes} — a refusal is an answer, not an error"
        }

        // The messaging path, to make the absence of a broker explicit.
        dynamic myota "EventDelivery" "How a fact reaches another service without a broker." {
            core -> coreDb "Writes the fact to ops.outbox_event in the domain transaction"
            activity -> core "Pulls GET /internal/events?after=<seq>"
            activity -> activityDb "Inserts an idempotent ops.job and advances the cursor"
        }

        // A role change and what it invalidates.
        dynamic myota "RoleChange" "An administrator grants a role, and the consumer learns." {
            administrator -> adminWeb "Grants a role from the Admins section"
            adminWeb -> gateway "POST /v1/admin/identity/admins/{id}/roles"
            gateway -> core "Forwards with the administrator session"
            core -> coreDb "Checks scopes ⊆ actor_scopes, writes account_role and the outbox event"
            geodata -> core "Pulls the role-changed event on its next cycle"
            geodata -> core "Asks again; the cached decision is no longer used"
        }

        deployment myota "Local K3s" "LocalK3s" "Deployment on the local K3s cluster." {
            include *
            autolayout lr
        }

        styles {
            element "Person" {
                shape Person
            }
            element "Database" {
                shape Cylinder
            }
            element "External" {
                background "#fbfcfb"
            }
            element "Static" {
                background "#fbfcfb"
            }
        }

    }
}
