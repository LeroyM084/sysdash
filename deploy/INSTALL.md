# Installer sysdash sur Debian 13

Ce guide part d'un serveur **Debian 13 (trixie) x86_64** et aboutit à un tableau de bord accessible en HTTPS, protégé par mot de passe. Suivez les étapes dans l'ordre.

Durée : 15 minutes environ.

---

## 0. Vue d'ensemble

```
Navigateur ──HTTPS + mot de passe──▶ Caddy ──▶ sysdash (:8090)
                                                  │
                                                  ├─ lit /proc, /sys (CPU, RAM, disques, réseau)
                                                  ├─ apt list --upgradable        (lecture seule)
                                                  └─ systemctl start apt-upgrade.service
                                                         │  autorisé par polkit, pour l'utilisateur
                                                         ▼  « sysdash » et cette unité uniquement
                                                     apt-get upgrade (exécuté par systemd, en root)
```

- sysdash tourne sous un utilisateur système dédié, **sans aucun privilège**.
- La mise à jour des paquets est exécutée par systemd (`apt-upgrade.service`), jamais par sysdash lui-même. Une règle polkit autorise `sysdash` à **démarrer** cette unité, et rien d'autre.
- Les index apt sont rafraîchis toutes les 6 h par `apt-refresh.timer`.

| Fichier du dépôt             | Destination sur le serveur |
|------------------------------|----------------------------|
| `target/release/sysdash`     | `/usr/local/bin/sysdash`   |
| `deploy/sysdash.service`     | `/etc/systemd/system/`     |
| `deploy/apt-refresh.service` | `/etc/systemd/system/`     |
| `deploy/apt-refresh.timer`   | `/etc/systemd/system/`     |
| `deploy/apt-upgrade.service` | `/etc/systemd/system/`     |
| `deploy/50-sysdash.rules`    | `/etc/polkit-1/rules.d/`   |

**Deux modes d'exposition** sont décrits au § 8 :

| Votre Caddy…                     | sysdash écoute sur | Section |
|----------------------------------|--------------------|---------|
| installé sur l'hôte (paquet apt) | `127.0.0.1:8090`   | § 8A    |
| dans un conteneur Docker         | `0.0.0.0:8090` + pare-feu | § 8B |

---

## 1. Prérequis

- Un serveur Debian 13 x86_64 avec systemd, joignable en SSH.
- Un compte avec `sudo`. Si `sudo` n'est pas installé :
  ```bash
  su -
  apt install sudo
  usermod -aG sudo VOTRE_UTILISATEUR
  exit   # puis se déconnecter et se reconnecter
  ```
- Un reverse proxy Caddy (sur l'hôte ou en Docker), ou la possibilité d'en installer un.
- Sur la machine de développement : Rust stable (`cargo`) et le dépôt sysdash.

Dans la suite, remplacez `moi@serveur` par votre utilisateur et l'adresse du serveur.

---

## 2. Compiler le binaire (machine de développement)

Compilez sur votre poste : le serveur n'a besoin d'aucune chaîne de compilation.

```bash
cd sysdash
cargo build --release
```

Le binaire produit est `target/release/sysdash` (≈ 1 Mo). Il contient toute l'interface web.

**Vérifier la compatibilité glibc.** Debian 13 fournit la glibc 2.41. Un binaire compilé sur une distribution plus récente (Fedora, Arch…) peut refuser de démarrer sur le serveur :

```bash
objdump -T target/release/sysdash | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1
```

- Résultat **≤ `GLIBC_2.41`** : c'est bon.
- Résultat supérieur : compilez un binaire statique musl, qui fonctionne sur n'importe quel Linux :
  ```bash
  rustup target add x86_64-unknown-linux-musl
  cargo build --release --target x86_64-unknown-linux-musl
  # binaire : target/x86_64-unknown-linux-musl/release/sysdash
  ```

**Optionnel : essayer en local.** Le mode « mock » simule les mises à jour, ce qui marche hors Debian :

```bash
SYSDASH_MOCK_UPDATES=1 ./target/release/sysdash
# puis ouvrir http://127.0.0.1:8090
```

**Copier les fichiers sur le serveur :**

```bash
ssh moi@serveur 'mkdir -p ~/sysdash-install'
scp target/release/sysdash deploy/* moi@serveur:~/sysdash-install/
```

---

## 3. Préparer le serveur

Toutes les commandes suivantes s'exécutent **sur le serveur**.

```bash
ssh moi@serveur
cd ~/sysdash-install
```

Installer les dépendances :

```bash
sudo apt update
sudo apt install -y polkitd curl
```

**`polkitd`** est indispensable. Une Debian minimale ne l'installe pas, et sans lui le bouton « Mettre à jour » renverra « Access denied ».

> **polkit « inactive », c'est normal.** polkit est une unité *statique* activée par D-Bus : elle démarre d'elle-même à la première demande d'autorisation. Inutile (et impossible) de faire `systemctl enable polkit`. Pour vérifier qu'elle démarre :
> ```bash
> sudo systemctl start polkit
> systemctl is-active polkit   # → active
> ```

---

## 4. Créer l'utilisateur système

Cet utilisateur n'a ni shell, ni répertoire personnel, ni mot de passe :

```bash
sudo useradd --system --user-group --no-create-home \
  --home-dir /nonexistent --shell /usr/sbin/nologin sysdash
```

Ajoutez-le au groupe `systemd-journal`, pour qu'il puisse lire les logs de mise à jour :

```bash
sudo usermod -aG systemd-journal sysdash
id sysdash   # doit lister systemd-journal
```

> L'unité contient aussi `SupplementaryGroups=systemd-journal`, mais cette directive ne s'applique qu'au service lancé par systemd. L'appartenance réelle au groupe rend en plus le test du § 7 (`sudo -u sysdash journalctl …`) représentatif.

---

## 5. Installer le binaire, les unités et la règle polkit

Le binaire appartient à root, si bien que le service ne peut pas le modifier :

```bash
sudo install -o root -g root -m 0755 sysdash /usr/local/bin/sysdash

sudo install -o root -g root -m 0644 \
  sysdash.service apt-refresh.service apt-refresh.timer apt-upgrade.service \
  /etc/systemd/system/

sudo install -o root -g root -m 0644 50-sysdash.rules /etc/polkit-1/rules.d/
sudo systemctl restart polkit
```

Vérifier que polkit a chargé la règle sans erreur :

```bash
sudo journalctl -u polkit -n 20 --no-pager
# attendu : « Loading rules from directory /etc/polkit-1/rules.d », sans erreur sur 50-sysdash.rules
```

Vérifier la syntaxe des unités (aucune erreur ne doit concerner ces quatre fichiers) :

```bash
sudo systemd-analyze verify /etc/systemd/system/sysdash.service \
  /etc/systemd/system/apt-refresh.service /etc/systemd/system/apt-refresh.timer \
  /etc/systemd/system/apt-upgrade.service
```

---

## 6. Activer les services

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now apt-refresh.timer
sudo systemctl enable --now sysdash.service
```

`apt-upgrade.service` ne s'active **pas** : il n'a pas de section `[Install]` et n'est démarré qu'à la demande, depuis le tableau de bord.

---

## 7. Vérifier l'installation

**Service et écoute.** Le service doit être `active (running)` et écouter sur `127.0.0.1:8090` :

```bash
systemctl status sysdash --no-pager
sudo ss -ltnp | grep 8090
```

**API.** Ctrl+C pour arrêter le flux :

```bash
curl -s http://127.0.0.1:8090/ | head -3              # du HTML
curl -sN http://127.0.0.1:8090/api/stream             # un JSON toutes les ~1,5 s
curl -s http://127.0.0.1:8090/api/updates; echo       # {"count":…,"packages":[…],…}
curl -s http://127.0.0.1:8090/api/updates/status; echo  # {"state":"idle","log":[]} au départ
```

**Accès au journal.** Aucun avertissement de permissions ne doit apparaître (« No entries » est normal tant qu'aucune mise à jour n'a tourné) :

```bash
sudo -u sysdash journalctl -u apt-upgrade.service -n 5 --no-pager
```

**Règle polkit : ce qui doit être refusé.** Les deux commandes doivent échouer avec « Access denied » :

```bash
sudo -u sysdash systemctl --no-ask-password stop apt-upgrade.service    # mauvais verbe
sudo -u sysdash systemctl --no-ask-password start apt-refresh.service   # mauvaise unité
```

L'autorisation positive (démarrer `apt-upgrade.service`) lance une vraie mise à jour : elle se teste depuis l'interface au § 9.

**Consommation mémoire** (cible < 10 Mo) :

```bash
ps -o rss=,comm= -C sysdash                            # RSS en Kio, attendu ≈ 3 000–5 000
systemctl show -p MemoryCurrent -p MemoryMax sysdash   # cgroup complet, en octets
```

---

## 8. Exposer avec Caddy

Ce tableau de bord peut lancer des mises à jour système en root : protégez-le toujours par mot de passe, et de préférence ne l'exposez **qu'en réseau local / VPN**, jamais via un tunnel public.

### 8A. Caddy installé sur l'hôte

sysdash garde son écoute par défaut sur `127.0.0.1:8090`, rien à changer côté sysdash.

**Installer Caddy** depuis le dépôt officiel (la directive `basic_auth` demande Caddy ≥ 2.8, le paquet Debian est plus ancien) :

```bash
sudo apt install -y gnupg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \
  | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \
  | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo chmod o+r /usr/share/keyrings/caddy-stable-archive-keyring.gpg \
  /etc/apt/sources.list.d/caddy-stable.list
sudo apt update
sudo apt install -y caddy
```

> Avec le paquet Debian (`sudo apt install caddy`), écrivez `basicauth` au lieu de `basic_auth`.

**Mot de passe :**

```bash
caddy hash-password
# saisir le mot de passe deux fois, copier le hash affiché ($2a$14$…)
```

**Site** dans `/etc/caddy/Caddyfile`, avec un domaine public :

```caddy
dash.example.com {
	basic_auth {
		admin $2a$14$REMPLACER_PAR_LE_HASH
	}
	reverse_proxy 127.0.0.1:8090
}
```

ou en réseau local, avec la CA interne de Caddy (avertissement navigateur à la première visite) :

```caddy
https://192.168.1.10 {
	tls internal
	basic_auth {
		admin $2a$14$REMPLACER_PAR_LE_HASH
	}
	reverse_proxy 127.0.0.1:8090
}
```

Dans `basic_auth`, le premier mot (`admin`) est le **nom d'utilisateur** demandé par le navigateur, le hash correspond au mot de passe.

```bash
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo systemctl reload caddy
```

Passez au § 8C.

### 8B. Caddy dans un conteneur Docker

Depuis un conteneur, `127.0.0.1` désigne le conteneur lui-même, pas l'hôte : Caddy ne peut pas joindre un sysdash qui n'écoute que sur `127.0.0.1` (symptôme : **502**). Il faut donc :

1. faire écouter sysdash sur toutes les interfaces ;
2. **filtrer avec le pare-feu** pour que seuls les réseaux Docker puissent joindre le port 8090 ;
3. donner au conteneur Caddy un nom pour joindre l'hôte.

> ⚠️ Sans pare-feu actif, `0.0.0.0:8090` expose sysdash **sans mot de passe** à tout le réseau local. Ne faites pas cette étape sans ufw (ou équivalent) configuré en refus par défaut.

**1. Écoute sur toutes les interfaces.** Créez un *drop-in* directement, sans éditeur (avec `systemctl edit`, un texte écrit dans la zone commentée est ignoré silencieusement) :

```bash
sudo mkdir -p /etc/systemd/system/sysdash.service.d
printf '[Service]\nEnvironment=SYSDASH_BIND=0.0.0.0:8090\n' \
  | sudo tee /etc/systemd/system/sysdash.service.d/bind.conf
sudo systemctl daemon-reload
sudo systemctl restart sysdash
sudo ss -ltnp | grep 8090   # → 0.0.0.0:8090
```

**2. Pare-feu (ufw).** Le trafic conteneur → hôte passe par la chaîne INPUT : ufw le bloque par défaut, comme le LAN. Il faut autoriser **chaque sous-réseau Docker** auquel Caddy est rattaché.

Lister les sous-réseaux Docker :

```bash
docker network inspect $(docker network ls -q) \
  -f '{{.Name}} {{range .IPAM.Config}}{{.Subnet}}{{end}}'
```

Lister les IP du conteneur Caddy (adaptez le nom du conteneur) :

```bash
docker inspect caddy -f '{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}'
```

Autoriser la plage `172.16.0.0/12`, qui couvre les réseaux Docker par défaut :

```bash
sudo ufw allow from 172.16.0.0/12 to any port 8090 proto tcp comment 'sysdash <- docker'
```

Si Caddy a aussi une IP **hors** de `172.16–31.x` (Docker bascule sur des plages `192.168.x.0/20` quand il a beaucoup de réseaux), ajoutez une règle pour **ce sous-réseau précis**, jamais pour tout `192.168.0.0/16` qui inclurait votre LAN :

```bash
# exemple : réseau Docker en 192.168.16.0/20
sudo ufw allow from 192.168.16.0/20 to any port 8090 proto tcp comment 'sysdash <- docker'
```

Vérifiez que la plage ajoutée ne chevauche pas votre LAN (ex. `192.168.16.0/20` = 192.168.16.0 → 192.168.31.255, sans conflit avec un LAN en `192.168.1.x`).

> Pourquoi toutes les IP ? Pour joindre l'hôte, le conteneur passe par sa route par défaut, qui peut sortir par n'importe lequel de ses réseaux. Couvrir chaque sous-réseau évite les surprises après un redéploiement.

**3. Compose de Caddy.** Ajoutez au service Caddy :

```yaml
services:
  caddy:
    extra_hosts:
      - "host.docker.internal:host-gateway"
```

Puis **recréez** le conteneur (un simple redémarrage ne suffit pas pour `extra_hosts`) :

```bash
docker compose up -d --force-recreate caddy
```

**4. Mot de passe :**

```bash
docker exec -it caddy caddy hash-password
```

**5. Site dans le Caddyfile.** Exemple en réseau local avec la CA interne de Caddy :

```caddy
sysdash.lan {
	tls internal
	basic_auth {
		admin $2a$14$REMPLACER_PAR_LE_HASH
	}
	reverse_proxy host.docker.internal:8090
}
```

Si vous avez déjà un snippet de filtrage par IP pour vos sites locaux, importez-le aussi dans ce bloc. Pensez à l'entrée DNS locale (`sysdash.lan` → IP du serveur) si vous n'avez pas de wildcard.

Rechargez Caddy :

```bash
docker exec caddy caddy reload --config /etc/caddy/Caddyfile --adapter caddyfile
```

**6. Tester depuis le conteneur :**

```bash
docker exec caddy getent hosts host.docker.internal                       # une IP (souvent 172.17.0.1)
docker exec caddy wget -qO- -T 3 http://host.docker.internal:8090/api/updates   # du JSON
```

| Résultat du `wget`      | Cause                                              |
|-------------------------|----------------------------------------------------|
| JSON                    | OK                                                 |
| `bad address`           | `extra_hosts` absent : recréez le conteneur        |
| `download timed out`    | ufw bloque : une IP de Caddy n'est pas couverte    |
| `Connection refused`    | sysdash écoute encore sur `127.0.0.1` (étape 1)    |

### 8C. Flux temps réel

Le flux SSE (`/api/stream`) marche avec Caddy **sans configuration particulière** : Caddy transmet les réponses `text/event-stream` immédiatement, sans tampon.

Le port 8090 ne doit **jamais** être ouvert au LAN ni à Internet : seuls 80/443 (ou le port de votre proxy) le sont.

---

## 9. Premier usage

Ouvrez l'adresse configurée et connectez-vous avec `admin` (ou le nom choisi dans `basic_auth`) et votre mot de passe.

- **Pastille verte** à côté du nom du serveur : le flux temps réel est connecté. Si elle est rouge, la page se reconnecte toute seule.
- **Carte « Mises à jour »** : cliquez sur **Mettre à jour**, puis **Confirmer**. Le journal d'`apt-get` défile en direct et la liste se rafraîchit à la fin. Le premier lancement réussi valide aussi la règle polkit.

Les montages `fuse.mergerfs` et autres systèmes de fichiers virtuels n'apparaissent volontairement pas dans la carte Disques : seuls les disques physiques sont listés, pour éviter les doubles comptages.

---

## 10. Configuration

Les réglages passent par des variables d'environnement, dans un *drop-in* (ne modifiez pas le fichier d'unité installé). Exemple :

```bash
sudo mkdir -p /etc/systemd/system/sysdash.service.d
printf '[Service]\nEnvironment=SYSDASH_INTERVAL_MS=3000\n' \
  | sudo tee /etc/systemd/system/sysdash.service.d/interval.conf
sudo systemctl daemon-reload
sudo systemctl restart sysdash
systemctl cat sysdash   # vérifier que le drop-in apparaît en bas
```

| Variable               | Défaut           | Rôle                                            |
|------------------------|------------------|-------------------------------------------------|
| `SYSDASH_BIND`         | `127.0.0.1:8090` | Adresse d'écoute (`0.0.0.0:8090` pour Caddy en Docker, § 8B) |
| `SYSDASH_INTERVAL_MS`  | `1500`           | Intervalle de rafraîchissement des statistiques |
| `SYSDASH_MOCK_UPDATES` | `0`              | `1` = mises à jour simulées (tests uniquement)  |

---

## 11. Mettre à jour sysdash

Après avoir recompilé (§ 2) et copié le nouveau binaire dans `~/sysdash-install/` :

```bash
sudo install -o root -g root -m 0755 ~/sysdash-install/sysdash /usr/local/bin/sysdash
sudo systemctl restart sysdash
```

Si des fichiers de `deploy/` ont changé, réinstallez-les comme au § 5, puis `sudo systemctl daemon-reload`. Les drop-ins de `/etc/systemd/system/sysdash.service.d/` sont conservés.

---

## 12. Dépannage

| Symptôme | Cause probable / solution |
|---|---|
| `systemctl is-active polkit` → `inactive` | Normal : polkit démarre à la demande (§ 3). `sudo systemctl start polkit` pour vérifier qu'il démarre. |
| `systemctl enable polkit` → « no installation config » | Normal : unité statique, rien à activer. |
| `journalctl` sans sudo → « No entries » + avertissement de permissions | Votre utilisateur n'est pas dans `systemd-journal`. Utilisez `sudo journalctl …` ou `sudo usermod -aG systemd-journal $USER` puis reconnectez-vous. |
| `sudo -u sysdash journalctl …` → « insufficient permissions » | `sysdash` n'est pas dans le groupe `systemd-journal` : `sudo usermod -aG systemd-journal sysdash` (§ 4). |
| « journal indisponible » dans l'interface | Vérifiez `SupplementaryGroups=systemd-journal` dans `systemctl cat sysdash`, puis `grep Groups /proc/$(pgrep -x sysdash)/status`. |
| « Lancement impossible : … Access denied » | Règle polkit non chargée ou `polkitd` absent. Vérifiez `ls /etc/polkit-1/rules.d/50-sysdash.rules` et `sudo journalctl -u polkit`. Le compte doit s'appeler exactement `sysdash`. |
| **502** derrière Caddy en Docker | Voir le tableau de diagnostic du § 8B (étape 6). |
| Un drop-in créé avec `systemctl edit` n'a aucun effet | Le texte a été écrit dans la zone commentée. Vérifiez avec `systemctl cat sysdash` ; créez plutôt le fichier avec `tee` (§ 8B, § 10). |
| Mise à jour en échec, « Could not get lock /var/lib/dpkg/lock-frontend » | Un autre apt tourne (ex. `unattended-upgrades`). Réessayez plus tard. Détail : `sudo journalctl -u apt-upgrade.service`. |
| Des paquets restent « à mettre à jour » après un succès | `apt-get upgrade` n'installe ni ne supprime de paquets (souvent le cas des nouveaux noyaux). Lancez de temps en temps `sudo apt full-upgrade` à la main. |
| La liste n'affiche pas une mise à jour toute récente | Cache de 5 min, index rafraîchis toutes les 6 h. Pour forcer : `sudo systemctl start apt-refresh.service`, puis `sudo systemctl restart sysdash`. |
| La liste des MAJ renvoie une erreur et `journalctl -u sysdash` mentionne un processus tué | `apt list` compte dans `MemoryMax=50M`. Augmentez via un drop-in (`MemoryMax=128M`). |
| Le badge « Redémarrage requis » n'apparaît jamais | Il reflète `/run/reboot-required`, créé notamment par le hook noyau d'`unattended-upgrades`. |
| La pastille reste rouge derrière un autre proxy que Caddy | Le proxy met le flux SSE en tampon. Avec nginx : `proxy_buffering off;`. |
| `sysdash` ne démarre pas, erreur `GLIBC_2.xx not found` | Binaire compilé avec une glibc trop récente : voir § 2 (build musl). |

Journaux utiles :

```bash
sudo journalctl -u sysdash -n 50 --no-pager
sudo journalctl -u apt-upgrade.service -n 100 --no-pager
systemctl list-timers apt-refresh.timer
```

---

## 13. Désinstaller

Vérifiez d'abord qu'aucune mise à jour n'est en cours (interrompre `dpkg` peut laisser le système incohérent) :

```bash
systemctl is-active apt-upgrade.service    # doit afficher : inactive (ou failed)
```

Puis :

```bash
sudo systemctl disable --now sysdash.service apt-refresh.timer
sudo rm -f /etc/systemd/system/sysdash.service \
  /etc/systemd/system/apt-refresh.service /etc/systemd/system/apt-refresh.timer \
  /etc/systemd/system/apt-upgrade.service
sudo rm -rf /etc/systemd/system/sysdash.service.d
sudo systemctl daemon-reload
sudo systemctl reset-failed

sudo rm -f /etc/polkit-1/rules.d/50-sysdash.rules
sudo systemctl restart polkit

sudo rm -f /usr/local/bin/sysdash
sudo userdel sysdash
sudo groupdel sysdash 2>/dev/null || true
rm -rf ~/sysdash-install
```

**Pare-feu** (si § 8B) : supprimez les règles du port 8090.

```bash
sudo ufw status numbered | grep 8090
sudo ufw delete <NUMÉRO>   # en partant du plus grand numéro
```

**Caddy** : retirez le bloc du site sysdash du Caddyfile (et `extra_hosts` du compose si plus rien ne s'en sert), puis rechargez Caddy.

`polkitd` peut rester installé. Retirez-le (`sudo apt purge polkitd`) seulement si rien d'autre ne l'utilise.
