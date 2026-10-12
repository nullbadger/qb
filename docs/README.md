# qb

**EMACS for Kubernetes.**

A keyboard-driven terminal UI for managing Kubernetes clusters. Single binary, native K8s API, no kubectl dependency.

```
┌ prod-cluster > All Namespaces > Deployments ───────────────── ⟳ just now ┐
├───────────────────────┬──────────────────────────────────────────────────┤
│  Overview             │ NAME             NAMESPACE    READY  UP-TO-DATE  │
│  Favorites (3)        │▶ api-gateway     production   3/3    3           │
│  Port Forwards        │  auth-service    production   2/2    2           │
│  Profiles             │  web-frontend    production   5/5    5           │
│ WORKLOADS             │  coredns         kube-system  2/2    2           │
│  Deployments (12)     │  metrics-server  monitoring   1/1    1           │
│  StatefulSets (2)     │                                                  │
│  Pods (24)            │                                                  │
│ NETWORK               │                                                  │
│  Services (8)         │                                                  │
│ CONFIG                │                                                  │
│  Secrets              │                                                  │
│ CERT-MANAGER.IO       │                                                  │
│  Certificate          │                                                  │
│  ClusterIssuer        │                                                  │
├───────────────────────┴──────────────────────────────────────────────────┤
│ e Edit  l Logs  D Delete  F PortFwd  * Star  / Filter  ? Help  q Quit    │
└──────────────────────────────────────────────────────────────────────────┘
```

## Why qb

Most Kubernetes dashboards are web apps that need a running backend, a browser, and a pile of YAML to deploy. `kubectl` is powerful but context-switching between commands and mentally tracking state across namespaces is slow.

qb gives you a single binary that connects directly to the K8s API server through your kubeconfig. You see everything at once -- resources, logs, port forwards, events -- and act on any of it without leaving the terminal. It discovers Custom Resources automatically, so third-party operators (cert-manager, Argo, Istio, etc.) show up in the sidebar with zero configuration.

## Features

### Browse

- **26 built-in resource types** across Workloads, Network, Config, Storage, RBAC, and Cluster categories
- **Custom Resources** discovered automatically from CRDs, grouped by API group in the sidebar
- **All namespaces by default** with a namespace column; narrow with `n`
- **Cluster overview** on startup: node grid cards, pod health gauges, cluster stats
- **Regex filter** (`/`) against name, namespace, and all column values
- **Resource count badges** in the sidebar
- **2-second auto-refresh** that preserves scroll position and selection

### Inspect

- **Smart detail views** with structured, per-resource-type rendering (replicas, conditions, ports, rules, volumes, etc.)
- **YAML view** toggle with `v`
- **Related events** shown at the bottom of every detail view (kubectl-describe style)
- **Related resources** -- navigate to owner references, pods, services with `r`
- **Labels and annotations** -- `l`/`a` to select, `Space` to expand long values, `y` to copy
- **Secret decoding** -- `Space` to decode base64 values in-place

### Act

| Key | Action | Scope |
|-----|--------|-------|
| `e` | Edit in `$EDITOR` with diff preview before apply | Any resource |
| `C` | Create from YAML template in `$EDITOR` | Any type |
| `D` | Delete with confirmation | Any resource |
| `S` | Scale replicas | Deployment, StatefulSet, ReplicaSet |
| `R` | Rolling restart | Deployment, StatefulSet, DaemonSet |
| `d` | Diff two resources side-by-side | Any two of same type |
| `K` | Cordon/uncordon | Node |
| `T` | Drain (cordon + evict pods) | Node |
| `T` | Trigger Job from CronJob | CronJob |

### Logs

- **All pods, all containers** aggregated by default for any workload
- **Color-coded by pod** -- each source pod gets a distinct prefix color
- **Strictly timestamp-ordered** -- lines from different pods merge correctly
- **Auto-discovery of new pods** -- rolling updates are captured without reopening the log view
- **Follow mode** (`f`) for live streaming
- **Regex filter** (`/`) with inline match highlighting
- **Pod/container selectors** (`p`/`c`) to narrow the stream
- **Time range** (`t`) -- e.g. `30m`, `2h`, `1d`
- **Line selection** with `j`/`k`, multi-select with `Shift+j`/`Shift+k`, copy with `Y`

### Port Forwarding

- **Create** (`F`) from Services, Deployments, StatefulSets, DaemonSets, ReplicaSets, Pods, Jobs, CronJobs
- **Survives pod restarts** -- resolves the current pod per-connection via label selectors
- **Persistent** -- saved to your active profile and restored on next launch
- **Manage** -- dedicated sidebar view to pause (`p`), resume, cancel (`d`), edit local port (`e`)

### Favorites and Profiles

- **Star** (`*`) any resource from list or detail view
- **Favorites view** in the sidebar -- fully functional (edit, logs, delete, etc.)
- **Profiles** -- named groups of favorites and port forwards; `Ctrl+s` to save, `P` to switch
- **Missing indicators** -- `⚠` for favorites from unreachable clusters

### Command Palette

- `Ctrl+p` opens actions for the current view. Type to filter, Enter runs the action.
- Not every action has a hotkey. Mutations (delete, restart, scale, cordon, drain, create, cancel forward) ask for confirmation before they run.
- A non-empty query also matches resources in the current list.
- Context-aware help with `?` (press `Tab` to see all keybindings)

### Exec (Experimental)

- `X` to exec into a pod (requires `qb -e` flag)
- Choose container, command, and terminal application in a dialog
- Opens a new terminal window with `kubectl exec -it`
- Supports Ghostty, iTerm2, Alacritty, kitty, WezTerm, Terminal.app, and others via `$TERMINAL` or `$TERM_PROGRAM`

## Installation

### From source

Requires [Rust](https://rustup.rs/) (edition 2021).

```sh
git clone https://github.com/cchexcode/qb.git
cd qb
cargo build --release -p qb
# Binary at target/release/qb
```

## Usage

```sh
qb                    # default kubeconfig and context
qb -e                 # enable experimental features (exec)
qb version            # print version
```

At runtime, press `c` to switch cluster context, `n` to switch namespace, or `O` to open a different kubeconfig file.

## Key Bindings

### Global

| Key | Action |
|-----|--------|
| `Ctrl+c` | Force quit |
| `q` | Quit / go back |
| `Esc` | Dismiss popup or selection |
| `?` | Help (context-aware; `Tab` for all) |
| `Ctrl+p` | Command palette |
| `Ctrl+s` | Save profile |
| `P` | Load/switch profile |
| `p` | Pause refresh is in the command palette |

### Main View

| Key | Action |
|-----|--------|
| `j`/`k` | Move down/up |
| `Ctrl+d`/`Ctrl+u` | Page down/up |
| `g`/`G` | Top/bottom |
| `Enter` | Open detail / focus table |
| `Tab` | Toggle sidebar/table focus |
| `<`/`>` | Resize sidebar |
| `c` | Switch cluster |
| `n` | Switch namespace |
| `O` | Open kubeconfig |
| `/` | Filter by regex |
| `x` | Clear filter |
| `e` | Edit resource |
| `l` | Logs |
| `D` | Delete |
| `R` | Restart |
| `S` | Scale |
| `F` | Port forward |
| `C` | Create resource |
| `d` | Mark/diff |
| `y` | Copy name |
| `*` | Toggle favorite |
| `X` | Exec (experimental) |

### Detail View

| Key | Action |
|-----|--------|
| `v` | Toggle Smart/YAML |
| `w` | Toggle watch (auto-refresh) |
| `l`/`a` | Select labels/annotations |
| `Space` | Expand value / decode secret |
| `y` | Copy value or YAML |
| `r` | Related resources |
| `e` | Edit |
| `L` | Logs |
| `D` | Delete |

### Log View

| Key | Action |
|-----|--------|
| `f` | Toggle follow |
| `/` | Filter by regex |
| `p`/`c` | Select pod/container |
| `t` | Time range |
| `w` | Toggle wrapping |
| `Y` | Copy to clipboard |
| `J`/`K` | Extend selection |

### Edit Diff View

| Key | Action |
|-----|--------|
| `v` | Toggle inline/side-by-side |
| `Enter` | Apply changes |
| `e` | Re-edit |
| `Esc` | Cancel |

### Port Forwards View

| Key | Action |
|-----|--------|
| `p` | Pause/resume |
| `d` | Cancel forward |
| `e` | Edit local port |

## How It Works

qb uses the [kube](https://github.com/kube-rs/kube) crate with [rustls](https://github.com/rustls/rustls) for direct communication with the Kubernetes API server. Authentication and cluster details come from your kubeconfig (`~/.kube/config` or `$KUBECONFIG`). There is no dependency on `kubectl` for any operation except exec (which spawns `kubectl exec` in a new terminal window).

The TUI is built on [ratatui](https://ratatui.rs/) + [crossterm](https://github.com/crossterm-rs/crossterm). Cluster calls run on background tasks; the event loop only draws, polls, and handles keys, so a slow or dead API cannot freeze the UI. Log follow is capped (bounded channel, 2,000 lines). After a sleep/wake the app drops follow streams and pauses refresh instead of replaying a backlog.

Custom Resource support works by discovering all CRDs at startup and using the kube dynamic API (`DynamicObject` + `ApiResource`) to list, inspect, edit, and delete CR instances without compile-time type information. CRD `additionalPrinterColumns` are used to render table columns.

Configuration (favorites, port forwards, profiles) is persisted to `~/.config/qb/config.yaml` and auto-saved on changes.

## License

MIT
