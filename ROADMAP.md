# sysdash — Spécification & roadmap pour agent IA

> Ce document est la source de vérité. Lis-le en entier avant de coder.
> Travaille **une phase à la fois**, dans l'ordre. À la fin de chaque phase : vérifie les critères d'acceptation, puis **arrête-toi** et résume ce qui a été fait avant de passer à la suivante.

---

## 1. Objectif

Dashboard web de monitoring serveur, minimaliste et joli, affichant **en direct** (sans historique persistant) :
- CPU et RAM
- Espace disque par point de montage réel
- Débit réseau up/down
- Mises à jour apt disponibles, avec possibilité de les appliquer

**Priorité n°1 : consommation RAM minimale** (cible < 10 Mo RSS en fonctionnement).

## 2. Contexte

- **Cible** : Debian 13 (trixie), x86_64, systemd, polkit. Derrière un reverse proxy Caddy.
- **Machine de dev** : Fedora (pas d'apt) → la partie mises à jour doit être **mockable** (voir §6).
- Livrable : **un seul binaire** contenant le front embarqué.

## 3. Contraintes techniques (obligatoires)

| Sujet | Choix imposé |
|---|---|
| Langage | Rust stable, edition 2024 |
| Runtime | `tokio` en **`current_thread`** (`#[tokio::main(flavor = "current_thread")]`) |
| HTTP / SSE | `axum` |
| Stats système | `sysinfo` |
| Front embarqué | `rust-embed` + `mime_guess` |
| Sérialisation | `serde`, `serde_json` |
| Front | HTML + CSS + JS **vanilla**. Pas de npm, pas de bundler, pas de framework, pas de CDN |

- N'ajoute **aucune autre dépendance** sans demander.
- Pas de `tracing` / logger lourd : `eprintln!` suffit.
- Pas de `unwrap()` dans le code serveur hors initialisation ; les erreurs d'API renvoient un JSON `{ "error": "..." }`.

Profil release dans `Cargo.toml` :
```toml
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
strip = true
panic = "abort"
```

## 4. Non-objectifs (ne pas implémenter)

- Historique, base de données, fichiers de stats
- Authentification (gérée par Caddy)
- Multi-serveurs, alertes, notifications
- Gestion de conteneurs Docker, processus, services

## 5. Arborescence attendue

```
sysdash/
├── Cargo.toml
├── src/
│   ├── main.rs        # config, routes, démarrage
│   ├── stats.rs       # collecte CPU/RAM/disques/réseau
│   ├── updates.rs     # trait UpdateBackend + impl Apt + impl Mock
│   └── web.rs         # service des fichiers embarqués
├── web/
│   ├── index.html
│   ├── style.css
│   └── app.js
└── deploy/
    ├── sysdash.service
    ├── apt-refresh.service
    ├── apt-refresh.timer
    ├── apt-upgrade.service
    ├── 50-sysdash.rules   # polkit
    └── INSTALL.md
```

## 6. Configuration (variables d'environnement)

| Variable | Défaut | Rôle |
|---|---|---|
| `SYSDASH_BIND` | `127.0.0.1:8090` | Adresse d'écoute |
| `SYSDASH_INTERVAL_MS` | `1500` | Intervalle de rafraîchissement des stats |
| `SYSDASH_MOCK_UPDATES` | `0` | `1` = backend de MAJ factice (dev hors Debian) |

## 7. Contrat d'API (à respecter exactement)

Toutes les tailles en **octets**, tous les débits en **octets/s**, pourcentages en `0.0–100.0`.

### `GET /api/stream` — SSE
Un événement `data:` par tick :
```json
{
  "ts": 1761062400,
  "hostname": "homelab",
  "uptime_s": 123456,
  "cpu": { "usage": 12.4, "cores": [10.1, 15.3, 8.0, 16.2] },
  "mem": { "used": 3221225472, "total": 16777216000, "swap_used": 0, "swap_total": 2147483648 },
  "disks": [
    { "device": "/dev/nvme0n1p2", "mount": "/", "fs": "ext4", "used": 51539607552, "total": 256060514304 }
  ],
  "net": { "rx_bps": 125000, "tx_bps": 32000 }
}
```

### `GET /api/updates`
```json
{
  "count": 2,
  "packages": [ { "name": "openssl", "current": "3.5.1-1", "candidate": "3.5.1-2" } ],
  "reboot_required": false,
  "checked_at": 1761062400
}
```

### `POST /api/updates/apply`
- `202` → lancement accepté
- `409` → une mise à jour est déjà en cours

### `GET /api/updates/status`
```json
{ "state": "idle", "log": ["ligne 1", "ligne 2"] }
```
`state` ∈ `idle` | `running` | `done` | `failed`.

---

## Phase 0 — Squelette

**Tâches**
1. `cargo new sysdash`, ajouter les dépendances du §3 et le profil release.
2. Arborescence du §5 (fichiers vides ou minimaux).
3. Lecture de la config §6.
4. Servir `web/index.html` sur `/` via `rust-embed`.

**Critères d'acceptation**
- `cargo build --release` passe sans warning.
- `cargo clippy -- -D warnings` passe.
- `curl -s localhost:8090/` renvoie le HTML.

---

## Phase 1 — Collecte des données

**Tâches**
1. `stats.rs` : une struct `Stats` sérialisable conforme au contrat §7.
2. Tâche tokio unique qui rafraîchit à `SYSDASH_INTERVAL_MS` et publie le dernier snapshot via `tokio::sync::watch`.
   - Ne rafraîchir **que** ce qui est nécessaire (`refresh_cpu_usage`, `refresh_memory`, `Disks::refresh`, `Networks::refresh`) — jamais `refresh_all`, jamais les processus.
3. **CPU** : usage global + par cœur. Le premier échantillon est invalide : faire un refresh initial puis attendre `sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`.
4. **Disques** :
   - Garder uniquement `ext4`, `xfs`, `btrfs`, `vfat`, `exfat`, `zfs`.
   - Exclure les montages sous `/var/lib/docker`, `/run`, `/snap`, `/boot/efi` est **gardé**.
   - Dédupliquer par `device` (garder le point de montage le plus court).
   - Trier par point de montage.
5. **Réseau** : somme des interfaces physiques. Exclure `lo`, `docker*`, `veth*`, `br-*`, `virbr*`, `tun*`, `wg*`. Débit = delta octets / intervalle réel écoulé (mesuré avec `Instant`, pas l'intervalle théorique).
6. Route `GET /api/stream` (SSE) : chaque client s'abonne au `watch`, envoie un keep-alive. Aucun calcul par client.

**Critères d'acceptation**
- `curl -N localhost:8090/api/stream` affiche un JSON conforme au §7 toutes les ~1,5 s.
- Les valeurs CPU/RAM sont cohérentes avec `htop` (±5 %).
- Lancer un téléchargement fait monter `rx_bps`.
- 5 clients SSE simultanés n'augmentent pas le CPU de manière notable.
- RSS du processus < 10 Mo (`ps -o rss= -p $(pgrep sysdash)`).

---

## Phase 2 — Mises à jour

**Tâches**
1. `updates.rs` : trait `UpdateBackend` avec `list()`, `apply()`, `status()`.
2. **`AptBackend`** :
   - `list()` : exécute `apt list --upgradable` avec `LANG=C`, ignore stderr et la ligne `Listing...`. Format d'une ligne : `nom/suite version arch [upgradable from: ancienne]`. Cache le résultat 5 min.
   - `reboot_required` = existence de `/run/reboot-required`.
   - `apply()` : vérifie l'état ; si `running` → erreur 409 ; sinon `systemctl start --no-block apt-upgrade.service`.
   - `status()` : `systemctl show -p ActiveState,Result apt-upgrade.service` → mapping `activating|active`→`running`, `failed`→`failed`, `inactive`+`Result=success` après une exécution→`done`, sinon `idle`. Logs : `journalctl -u apt-upgrade.service -n 50 -o cat --no-pager`.
   - Invalider le cache de `list()` quand une MAJ se termine.
3. **`MockBackend`** (si `SYSDASH_MOCK_UPDATES=1`) : 3 paquets factices, `apply()` simule 10 s de `running` avec des lignes de log, puis `done`.
4. Routes du §7.
5. Fichiers `deploy/` :
   - `apt-refresh.service` + `apt-refresh.timer` : `apt-get update` toutes les 6 h, `Persistent=true`.
   - `apt-upgrade.service` : `Type=oneshot`, `Environment=DEBIAN_FRONTEND=noninteractive`, `ExecStart=/usr/bin/apt-get -y -o Dpkg::Options::=--force-confold upgrade`.
   - `50-sysdash.rules` : autorise **uniquement** l'utilisateur `sysdash` à faire `start` sur `apt-upgrade.service` (`org.freedesktop.systemd1.manage-units`, vérifier `unit` et `verb`).

**Règles de sécurité (non négociables)**
- L'application n'exécute **jamais** `apt`, `apt-get` ou `sudo` directement avec des privilèges.
- Aucune entrée utilisateur ne doit être passée à une commande shell. Les commandes sont des constantes, lancées via `std::process::Command` (pas de `sh -c`).

**Critères d'acceptation**
- En mode mock sur Fedora : la liste s'affiche, `apply` passe en `running` puis `done`, un second `apply` pendant l'exécution renvoie `409`.
- Tests unitaires du parseur `apt list` (au moins : ligne normale, ligne `Listing...`, sortie vide, ligne malformée ignorée).
- `cargo test` passe.

---

## Phase 3 — Design & affichage web

**Direction visuelle**
- Dark mode uniquement, fond `#0d1117`, cartes `#161b22`, bordures `#30363d`, texte `#e6edf3`, texte secondaire `#8b949e`.
- Accent par métrique : CPU `#58a6ff`, RAM `#bc8cff`, disques `#3fb950`, réseau ↓ `#39d0d8` / ↑ `#f0883e`.
- Seuils : vert < 80 %, orange `#d29922` ≥ 80 %, rouge `#f85149` ≥ 90 %.
- Chiffres en `font-family: ui-monospace, "JetBrains Mono", "Cascadia Code", monospace;` avec `font-variant-numeric: tabular-nums`.
- Coins arrondis 12 px, espacement généreux, aucune ombre lourde.
- Responsive : grille 2-3 colonnes sur desktop, 1 colonne sous 700 px.

**Composants**
1. **En-tête** : hostname, uptime, pastille de connexion (verte = SSE ok, rouge = déconnecté).
2. **CPU** : jauge circulaire SVG (`stroke-dasharray` animé) + pourcentage au centre ; mini-barres par cœur en dessous.
3. **RAM** : jauge circulaire + `utilisé / total` en Go ; swap en petite barre.
4. **Disques** : une ligne par disque — point de montage, barre de remplissage, `utilisé / total`, pourcentage.
5. **Réseau** : valeurs ↓/↑ formatées (Ko/s, Mo/s) + deux sparklines SVG sur les **60 derniers points gardés en mémoire JS uniquement**.
6. **Mises à jour** : badge avec le nombre ; liste dépliable (nom, version actuelle → nouvelle) ; badge "Redémarrage requis" si besoin ; bouton "Mettre à jour" → confirmation → pendant `running`, poll de `/api/updates/status` toutes les 2 s avec logs dans un bloc `<pre>` auto-scrollé ; à la fin, rafraîchir la liste.

**Comportement**
- `EventSource('/api/stream')` avec reconnexion auto et pastille d'état.
- Transitions CSS (`transition: 300ms ease`) sur jauges et barres.
- Formatage des tailles en binaire (Kio/Mio/Gio affichés "Ko/Mo/Go"), 1 décimale.
- Aucun `innerHTML` avec des données serveur non échappées (noms de paquets inclus) : utiliser `textContent`.

**Critères d'acceptation**
- La page est lisible et cohérente sur 1920 px et 390 px de large.
- Couper le serveur → pastille rouge ; le relancer → reconnexion automatique.
- Le flow de mise à jour complet fonctionne en mode mock.
- Aucune requête vers un domaine externe (vérifier l'onglet Réseau du navigateur).

---

## Phase 4 — Déploiement

**Tâches**
1. `deploy/sysdash.service` :
   - `User=sysdash`, `Group=sysdash`, `SupplementaryGroups=systemd-journal`
   - `Environment=SYSDASH_BIND=127.0.0.1:8090`
   - Durcissement : `NoNewPrivileges=true`, `ProtectSystem=strict`, `ProtectHome=true`, `PrivateTmp=true`, `PrivateDevices=true`, `ProtectKernelTunables=true`, `ProtectKernelModules=true`, `ProtectControlGroups=true`, `RestrictSUIDSGID=true`, `MemoryMax=50M`
   - `Restart=on-failure`
2. `deploy/INSTALL.md` : commandes pas à pas pour Debian 13 —
   - créer l'utilisateur système `sysdash` (sans shell, sans home)
   - copier le binaire dans `/usr/local/bin/`
   - copier les unités dans `/etc/systemd/system/`, la règle polkit dans `/etc/polkit-1/rules.d/`
   - `systemctl daemon-reload`, activer `apt-refresh.timer` et `sysdash.service`
   - exemple de bloc Caddy (`reverse_proxy 127.0.0.1:8090` + `basic_auth`) — préciser que SSE fonctionne sans config spéciale avec Caddy
   - commande de vérification de la RAM
   - procédure de désinstallation

**Critères d'acceptation**
- `systemd-analyze verify deploy/*.service deploy/*.timer` ne remonte pas d'erreur bloquante.
- `INSTALL.md` est suivable de bout en bout sans connaissance préalable du projet.

---

## Règles générales pour l'agent

- **Ne modifie jamais le système de la machine de dev** : pas de `sudo`, pas d'installation d'unités systemd, pas de `apt`/`dnf`. Tout ce qui touche au système va dans `deploy/` et c'est l'humain qui installe.
- Après chaque modification de code : `cargo fmt`, `cargo clippy -- -D warnings`, `cargo build`.
- Si une contrainte de ce document est impossible ou incohérente, **arrête-toi et pose la question** plutôt que d'improviser.
- Pas de fonctionnalité hors spec, même "petite".
- Commit à la fin de chaque phase : `phase N: <résumé>`.
