# Installer sysdash sur Debian 13

Ce guide part d'un serveur **Debian 13 (trixie) x86_64 fraîchement installé** et aboutit à un tableau de bord accessible en HTTPS, protégé par mot de passe. Aucune connaissance préalable du projet n'est nécessaire : suivez les étapes dans l'ordre.

Durée : 15 minutes environ.

---

## 0. Vue d'ensemble

```
Navigateur ──HTTPS + mot de passe──▶ Caddy (:443) ──▶ sysdash (127.0.0.1:8090)
                                                          │
                                                          ├─ lit /proc, /sys (CPU, RAM, disques, réseau)
                                                          ├─ apt list --upgradable        (lecture seule)
                                                          └─ systemctl start apt-upgrade.service
                                                                 │  autorisé par polkit, pour l'utilisateur
                                                                 ▼  « sysdash » et cette unité uniquement
                                                             apt-get upgrade (exécuté par systemd, en root)
```

- sysdash tourne sous un utilisateur système dédié, **sans aucun privilège**. Il n'écoute que sur `127.0.0.1` : seul Caddy y accède.
- La mise à jour des paquets est exécutée par systemd (`apt-upgrade.service`), jamais par sysdash lui-même. Une règle polkit autorise `sysdash` à **démarrer** cette unité, et rien d'autre.
- Les index apt sont rafraîchis toutes les 6 h par `apt-refresh.timer`.

| Fichier du dépôt | Destination sur le serveur |
|---|---|
| `target/release/sysdash` | `/usr/local/bin/sysdash` |
| `deploy/sysdash.service` | `/etc/systemd/system/` |
| `deploy/apt-refresh.service` | `/etc/systemd/system/` |
| `deploy/apt-refresh.timer` | `/etc/systemd/system/` |
| `deploy/apt-upgrade.service` | `/etc/systemd/system/` |
| `deploy/50-sysdash.rules` | `/etc/polkit-1/rules.d/` |

---

## 1. Prérequis

- Un serveur Debian 13 x86_64 avec systemd, joignable en SSH.
- Un compte avec `sudo`. Si le mot de passe root a été défini à l'installation, `sudo` n'est pas installé. Dans ce cas :
  ```sh
  su -
  apt install sudo
  usermod -aG sudo VOTRE_UTILISATEUR
  exit   # puis se déconnecter et se reconnecter
  ```
- **Pour un accès depuis Internet** : un nom de domaine (ex. `dash.example.com`) dont l'enregistrement DNS A/AAAA pointe vers le serveur, et les ports 80 et 443 ouverts. Caddy obtiendra le certificat HTTPS tout seul.
  Pour un usage en réseau local uniquement, voir la variante au § 8.4.
- Sur la machine de développement : Rust stable (`cargo`) et le dépôt sysdash.

Dans la suite, remplacez :
- `dash.example.com` par votre nom de domaine ;
- `moi@serveur` par votre utilisateur et l'adresse du serveur.

---

## 2. Compiler le binaire (machine de développement)

Compilez sur votre poste plutôt que sur le serveur, qui n'a ainsi besoin d'aucune chaîne de compilation.

```sh
cd sysdash
cargo build --release
```

Le binaire produit est `target/release/sysdash` (≈ 1 Mo). Il contient déjà toute l'interface web.

**Vérifier la compatibilité glibc.** Debian 13 fournit la glibc 2.41. Un binaire compilé sur une distribution plus récente (Fedora, Arch…) peut exiger une version supérieure et refuser de démarrer sur le serveur :

```sh
objdump -T target/release/sysdash | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1
```

- Résultat **≤ `GLIBC_2.41`** : c'est bon.
- Résultat supérieur : produisez plutôt un binaire statique avec musl, qui fonctionne sur n'importe quel Linux :
  ```sh
  rustup target add x86_64-unknown-linux-musl
  cargo build --release --target x86_64-unknown-linux-musl
  # binaire : target/x86_64-unknown-linux-musl/release/sysdash
  ```

**Optionnel : essayer en local.** Le mode « mock » simule les mises à jour, ce qui marche aussi hors Debian :

```sh
SYSDASH_MOCK_UPDATES=1 ./target/release/sysdash
# puis ouvrir http://127.0.0.1:8090
```

**Copier les fichiers sur le serveur :**

```sh
ssh moi@serveur 'mkdir -p ~/sysdash-install'
scp target/release/sysdash deploy/* moi@serveur:~/sysdash-install/
```

---

## 3. Préparer le serveur

Toutes les commandes suivantes s'exécutent **sur le serveur**.

```sh
ssh moi@serveur
cd ~/sysdash-install
```

Mettre le système à jour et installer les dépendances :

```sh
sudo apt update
sudo apt full-upgrade -y
sudo apt install -y polkitd curl gnupg
```

- **`polkitd`** est indispensable. Une Debian minimale ne l'installe pas, et sans lui le bouton « Mettre à jour » renverra « Access denied ».
- `curl` et `gnupg` servent aux vérifications et à l'installation de Caddy.

Vérifier que polkit tourne :

```sh
systemctl is-active polkit   # doit afficher : active
```

> Si le serveur vient d'installer un nouveau noyau, redémarrez maintenant (`sudo reboot`), puis reprenez à l'étape suivante.

---

## 4. Créer l'utilisateur système

Cet utilisateur n'a ni shell, ni répertoire personnel, ni mot de passe :

```sh
sudo useradd --system --user-group --no-create-home \
  --home-dir /nonexistent --shell /usr/sbin/nologin sysdash
id sysdash
```

Il n'est **pas** nécessaire de l'ajouter au groupe `systemd-journal` : l'unité systemd le fait (`SupplementaryGroups=`).

---

## 5. Installer le binaire, les unités et la règle polkit

Le binaire appartient à root, si bien que le service ne peut pas le modifier :

```sh
sudo install -o root -g root -m 0755 sysdash /usr/local/bin/sysdash

sudo install -o root -g root -m 0644 \
  sysdash.service apt-refresh.service apt-refresh.timer apt-upgrade.service \
  /etc/systemd/system/

sudo install -o root -g root -m 0644 50-sysdash.rules /etc/polkit-1/rules.d/
sudo systemctl restart polkit
```

Vérifier la syntaxe des unités. Aucune erreur ne doit concerner ces quatre fichiers :

```sh
sudo systemd-analyze verify /etc/systemd/system/sysdash.service \
  /etc/systemd/system/apt-refresh.service /etc/systemd/system/apt-refresh.timer \
  /etc/systemd/system/apt-upgrade.service
```

---

## 6. Activer les services

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now apt-refresh.timer
sudo systemctl enable --now sysdash.service
```

`apt-upgrade.service` ne s'active **pas** : il n'a pas de section `[Install]` et n'est démarré qu'à la demande, depuis le tableau de bord.

---

## 7. Vérifier l'installation

**Service et écoute.** Le service doit être `active (running)`, et l'adresse `127.0.0.1:8090` uniquement (jamais `0.0.0.0`) :

```sh
systemctl status sysdash --no-pager
sudo ss -ltnp | grep 8090
```

**API.** Faire Ctrl+C pour arrêter le flux :

```sh
curl -s http://127.0.0.1:8090/ | head -3              # du HTML
curl -sN http://127.0.0.1:8090/api/stream             # un JSON toutes les ~1,5 s
curl -s http://127.0.0.1:8090/api/updates; echo       # {"count":…,"packages":[…],…}
curl -s http://127.0.0.1:8090/api/updates/status; echo
```

**Accès au journal.** Ne doit **pas** afficher d'avertissement de permissions (« No entries » est normal) :

```sh
sudo -u sysdash journalctl -u apt-upgrade.service -n 5 --no-pager
```

**Règle polkit : ce qui doit être refusé.** Les deux commandes doivent échouer avec « Access denied » :

```sh
sudo -u sysdash systemctl --no-ask-password stop apt-upgrade.service    # mauvais verbe
sudo -u sysdash systemctl --no-ask-password start apt-refresh.service   # mauvaise unité
```

L'autorisation positive (démarrer `apt-upgrade.service`) se teste depuis l'interface au § 9.

**Consommation mémoire** (cible < 10 Mo) :

```sh
ps -o rss=,comm= -C sysdash                                  # RSS en Kio, attendu ≈ 3 000–5 000
systemctl show -p MemoryCurrent -p MemoryMax sysdash         # cgroup complet, en octets
```

---

## 8. Exposer avec Caddy

### 8.1 Installer Caddy

Utilisez le dépôt officiel de Caddy : la directive `basic_auth` utilisée ci-dessous demande Caddy ≥ 2.8, et le paquet Debian est plus ancien.

```sh
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \
  | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \
  | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo chmod o+r /usr/share/keyrings/caddy-stable-archive-keyring.gpg \
  /etc/apt/sources.list.d/caddy-stable.list
sudo apt update
sudo apt install -y caddy
caddy version
```

> Si vous préférez le paquet Debian (`sudo apt install caddy`), écrivez `basicauth` au lieu de `basic_auth`.

### 8.2 Créer le mot de passe

Ce tableau de bord peut lancer des mises à jour système : choisissez un **mot de passe long et unique**.

```sh
caddy hash-password
# saisir le mot de passe deux fois, puis copier le hash affiché ($2a$14$…)
```

### 8.3 Configurer le site

Remplacez le contenu de `/etc/caddy/Caddyfile` (`sudo nano /etc/caddy/Caddyfile`) :

```caddyfile
dash.example.com {
	basic_auth {
		admin $2a$14$REMPLACER_PAR_LE_HASH
	}
	reverse_proxy 127.0.0.1:8090
}
```

Le flux temps réel (SSE, `/api/stream`) marche **sans configuration particulière** : Caddy détecte les réponses `text/event-stream` et les transmet immédiatement, sans les mettre en tampon.

Valider puis appliquer :

```sh
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo systemctl reload caddy
journalctl -u caddy -n 30 --no-pager      # vérifier l'obtention du certificat
```

### 8.4 Variante : réseau local sans nom de domaine

Sans domaine public, Caddy peut servir en HTTPS avec sa propre autorité de certification. Le navigateur affichera un avertissement à la première visite.

```caddyfile
https://192.168.1.10 {
	tls internal
	basic_auth {
		admin $2a$14$REMPLACER_PAR_LE_HASH
	}
	reverse_proxy 127.0.0.1:8090
}
```

Évitez le HTTP simple (`http://…`) : l'authentification basique y circule en clair.

### 8.5 Pare-feu

Debian n'active pas de pare-feu par défaut. Si vous en utilisez un, ouvrez uniquement 80/tcp et 443/tcp (`sudo ufw allow 80,443/tcp` avec ufw). Le port 8090 ne doit **jamais** être ouvert.

---

## 9. Premier usage

Ouvrez `https://dash.example.com` et connectez-vous avec `admin` et votre mot de passe.

- **Pastille verte** à côté du nom du serveur : le flux temps réel est connecté. Si elle est rouge, la page se reconnecte toute seule.
- **Carte « Mises à jour »** : cliquez sur **Mettre à jour**, puis **Confirmer**. Le journal d'`apt-get` défile en direct, et la liste se rafraîchit à la fin.
  Le premier lancement réussi valide aussi la règle polkit.

---

## 10. Configuration

Les réglages passent par des variables d'environnement. Modifiez-les avec un *drop-in*, sans toucher au fichier d'unité installé :

```sh
sudo systemctl edit sysdash
```

```ini
[Service]
Environment=SYSDASH_INTERVAL_MS=3000
```

```sh
sudo systemctl restart sysdash
```

| Variable | Défaut | Rôle |
|---|---|---|
| `SYSDASH_BIND` | `127.0.0.1:8090` | Adresse d'écoute (gardez `127.0.0.1`) |
| `SYSDASH_INTERVAL_MS` | `1500` | Intervalle de rafraîchissement des statistiques |
| `SYSDASH_MOCK_UPDATES` | `0` | `1` = mises à jour simulées (tests uniquement) |

---

## 11. Mettre à jour sysdash

Après avoir recompilé (§ 2) et copié le nouveau binaire dans `~/sysdash-install/` :

```sh
sudo install -o root -g root -m 0755 ~/sysdash-install/sysdash /usr/local/bin/sysdash
sudo systemctl restart sysdash
```

Si des fichiers de `deploy/` ont changé, réinstallez-les comme au § 5, puis lancez `sudo systemctl daemon-reload`.

---

## 12. Dépannage

| Symptôme | Cause probable / solution |
|---|---|
| « Lancement impossible : … Access denied » | Règle polkit non chargée ou `polkitd` absent. Vérifiez `systemctl is-active polkit` et `ls /etc/polkit-1/rules.d/50-sysdash.rules`, consultez `journalctl -u polkit`. Le compte doit s'appeler exactement `sysdash`. |
| « journal indisponible » dans les logs | sysdash n'arrive pas à lire le journal. Vérifiez la ligne `SupplementaryGroups=systemd-journal` dans `systemctl cat sysdash`. |
| Mise à jour en échec, « Could not get lock /var/lib/dpkg/lock-frontend » | Un autre apt tourne (ex. `unattended-upgrades`). Réessayez quelques minutes plus tard. Détail : `journalctl -u apt-upgrade.service`. |
| Des paquets restent « à mettre à jour » après un succès | `apt-get upgrade` n'installe ni ne supprime de paquets : c'est souvent le cas des nouveaux noyaux. Lancez de temps en temps `sudo apt full-upgrade` à la main. |
| La liste n'affiche pas une mise à jour toute récente | La liste est mise en cache 5 min et les index sont rafraîchis toutes les 6 h. Pour forcer : `sudo systemctl start apt-refresh.service`, puis `sudo systemctl restart sysdash`. |
| La liste des mises à jour renvoie une erreur et `journalctl -u sysdash` mentionne un processus tué | `apt list` tourne dans le cgroup de sysdash et compte dans `MemoryMax=50M`. Avec de très nombreuses sources apt, augmentez la limite via `sudo systemctl edit sysdash` (`MemoryMax=128M`). |
| Le badge « Redémarrage requis » n'apparaît jamais | Il reflète la présence de `/run/reboot-required`. Sur Debian, ce fichier n'est pas créé systématiquement : c'est notamment le hook noyau du paquet `unattended-upgrades` qui le crée. |
| La pastille reste rouge derrière un autre proxy que Caddy | Le proxy met le flux SSE en tampon. Avec nginx, par exemple : `proxy_buffering off;`. |
| `sysdash` ne démarre pas, erreur `GLIBC_2.xx not found` | Binaire compilé avec une glibc trop récente. Voir § 2 (vérification glibc ou build musl). |

Journaux utiles :

```sh
journalctl -u sysdash -n 50 --no-pager
journalctl -u apt-upgrade.service -n 100 --no-pager
systemctl list-timers apt-refresh.timer
```

---

## 13. Désinstaller

Vérifiez d'abord qu'aucune mise à jour n'est en cours. Interrompre `dpkg` peut laisser le système dans un état incohérent :

```sh
systemctl is-active apt-upgrade.service    # doit afficher : inactive (ou failed)
```

Puis :

```sh
sudo systemctl disable --now sysdash.service apt-refresh.timer
sudo rm -f /etc/systemd/system/sysdash.service \
  /etc/systemd/system/apt-refresh.service /etc/systemd/system/apt-refresh.timer \
  /etc/systemd/system/apt-upgrade.service
sudo rm -rf /etc/systemd/system/sysdash.service.d      # drop-ins éventuels (§ 10)
sudo systemctl daemon-reload
sudo systemctl reset-failed

sudo rm -f /etc/polkit-1/rules.d/50-sysdash.rules
sudo systemctl restart polkit

sudo rm -f /usr/local/bin/sysdash
sudo userdel sysdash
sudo groupdel sysdash 2>/dev/null || true
rm -rf ~/sysdash-install
```

Côté Caddy, retirez le bloc `dash.example.com { … }` du Caddyfile, puis `sudo systemctl reload caddy`.
Pour supprimer entièrement Caddy :

```sh
sudo apt purge -y caddy
sudo rm -f /etc/apt/sources.list.d/caddy-stable.list \
  /usr/share/keyrings/caddy-stable-archive-keyring.gpg
sudo apt update
```

`polkitd` peut rester installé. Retirez-le (`sudo apt purge polkitd`) seulement si rien d'autre ne l'utilise.
