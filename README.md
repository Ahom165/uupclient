# UUP dump Client — client natif en Rust

Client de bureau **natif, léger et sobre** pour [UUP dump](https://uupdump.net) :
recherche des builds Windows (Insider, Retail, Server…), téléchargement des fichiers UUP
directement depuis les serveurs Microsoft, intégration de drivers, et création d'ISO
en s'appuyant sur le **pipeline de conversion officiel** d'UUP dump.

Pas de webview, pas d'Electron, pas d'aria2 à installer : un seul binaire Rust
(egui/eframe) avec téléchargeur intégré.

---

## Fonctionnalités

- **Recherche de builds** : par mots-clés ou n° de build (`listid.php`), ou « dernières
  builds » par canal — Canary, Dev, Beta, Release Preview, Retail (`fetchupd.php`),
  pour les architectures `amd64`, `arm64` et `x86`.
- **Sélection langue + éditions** : langues et éditions lues depuis l'API officielle ;
  multi-éditions possible (les fichiers communs sont dédoublonnés).
- **Deux modes de téléchargement** :
  - *Set complet* (image d'installation convertible en ISO) ;
  - *Mises à jour uniquement* (`UPDATEONLY` : cabinets/MSU de la build, sans image).
- **Téléchargeur natif parallèle** : N connexions simultanées, **reprise via HTTP Range**
  (fichiers `.part`), **vérification SHA-1** optionnelle, progression par fichier,
  annulation et reprise d'une session à l'autre.
- **Boutons de paramètres** pour la création :
  - Convertir en ISO (on/off)
  - Intégrer les mises à jour cumulatives (`AddUpdates`)
  - **Auto-clean** (`Cleanup`) : suppression des fichiers temporaires après création
  - ResetBase, SkipEdge, compression **ESD** (WIM par défaut), éditions virtuelles
- **Add Driver** : ajout de plusieurs dossiers de drivers `.inf` depuis l'UI ; ils sont
  centralisés dans `Drivers/` du projet puis intégrés à `install.wim` via les
  paramètres officiels `AddDrivers` / `Drv_Source`.
- **Création d'ISO orchestrée** : récupération du package de conversion officiel
  (`autodl=2`), téléchargement du convertisseur avec **vérification SHA-256**,
  extraction (7zr officiel sous Windows), patch automatique de `ConvertConfig.ini`
  selon tes choix, puis lancement de la conversion (élévation administrateur).
- **Persistance** : réglages, drivers et destination sauvegardés dans
  `settings.json` (répertoire de configuration de l'OS).
- **Journal intégré** : chaque étape est tracée dans le panneau « Journal ».

---

## Compilation

### Windows (recommandé — cible principale)

1. Installe Rust : [rustup-init.exe](https://win.rustup.rs/x86_64) (toolchain `stable`,
   composant `msvc` + Visual Studio Build Tools si demandé).
2. Dans le dossier du projet :

```bat
cargo build --release
```

3. Le binaire se trouve dans `target\release\uupdump-client.exe` (~15 Mo, autonome).

### Linux

```bash
cargo build --release
./target/release/uupdump-client
```

### macOS

Identique à Linux (`cargo build --release`). La conversion ISO nécessite les outils du
convertisseur multiplateforme (voir plus bas).

---

## Utilisation

1. **⚙ Paramètres** → choisis le **dossier de destination** et ajuste les options
   (threads, vérification SHA-1, options de création, **drivers**).
2. Écran principal : saisis un n° de build / mots-clés puis **Rechercher**
   (ou **Dernières du canal** avec le canal choisi).
3. Clique sur une build → choisis le mode (set complet ou mises à jour seules),
   la **langue**, coche les **éditions** (Windows Pro cochée par défaut).
4. **Télécharger** → progression globale et par fichier ; annulation et reprise possibles.
5. À la fin du téléchargement, la **création de l'ISO démarre automatiquement**
   (si « Convertir en ISO » est actif) : le convertisseur officiel s'exécute dans une
   fenêtre séparée — accepte l'élévation administrateur (DISM).
6. Quand l'ISO est prête : chemin affiché, bouton **Ouvrir le dossier**,
   et auto-clean des fichiers temporaires si activé.

### Ajouter des drivers

Paramètres → **« + Ajouter un dossier de drivers »** (autant que nécessaire).
Au moment du build, chaque dossier est copié vers `<projet>/Drivers/…` et le
`ConvertConfig.ini` est patché avec `AddDrivers=1` et `Drv_Source=\Drivers` —
le convertisseur officiel les intègre alors à `install.wim`.

---

## Options de création (traduites du ConvertConfig.ini officiel)

| Option UI | Clé INI | Effet |
|---|---|---|
| Convertir en ISO | — | Si désactivé : seuls les fichiers UUP sont téléchargés |
| Intégrer les mises à jour | `AddUpdates` | Applique la cumulative LCU + SSU dans l'image |
| Auto-clean | `Cleanup` | Supprime les temporaires du convertisseur ; l'app supprime aussi `UUPs/` et `files/` après succès |
| ResetBase | `ResetBase` | Compacte la base de composants (plus long, ISO plus petite) |
| Ne pas réintégrer Edge | `SkipEdge` | Évite de réinstaller Edge dans l'image |
| Compression ESD | `wim2esd` | `install.esd` (taille réduite) au lieu de `install.wim` |
| Éditions virtuelles | `StartVirtual` | Génère Enterprise, Education… depuis Pro |
| Drivers | `AddDrivers` + `Drv_Source` | Intégration des dossiers `.inf` dans `install.wim` |

---

## Comment ça marche (architecture)

```
┌────────────────────────── uupdump-client (Rust) ──────────────────────────┐
│  UI egui sobre (recherche → détail → téléchargement → ISO → journal)      │
│                                                                            │
│  api.rs      listid / fetchupd / listlangs / listeditions / get            │
│              (api.uupdump.net, retry auto sur 429/5xx, erreurs traduites)  │
│  downloader  téléchargeur natif : threads + HTTP Range + SHA-1 + annulation│
│              → écrit <projet>/UUPs/ (même disposition que l'officiel)      │
│  builder     package officiel autodl=2 → fichiers converter (SHA-256)      │
│              → extraction 7z → patch ConvertConfig.ini → conversion        │
│              → surveillance ISO → auto-clean                               │
│  config      settings.json (drivers, options, destination, canal/arch)     │
└────────────────────────────────────────────────────────────────────────────┘
```

Le pipeline de conversion est **celui d'UUP dump** : l'application télécharge le
même package que le site (`get.php?...&autodl=2`), le même convertisseur
(`uup-converter-wimlib`, empreinte vérifiée) et applique tes options dans
`ConvertConfig.ini` — exactement comme si tu avais généré le package sur le site,
mais avec un téléchargeur natif et une UI dédiée.

### Fichiers générés

```
<destination>/
└── 29671.1000_amd64_fr-fr_professional_<uuid8>/   ← projet
    ├── UUPs/                  ← fichiers UUP téléchargés
    ├── files/                 ← outils du convertisseur (temporaires)
    ├── Drivers/               ← drivers centralisés (si utilisés)
    ├── ConvertConfig.ini      ← patché selon tes choix
    ├── convert-UUP.cmd        ← convertisseur officiel (Windows)
    └── <nom>.iso              ← résultat
```

Réglages : `%APPDATA%\uupdump-client\settings.json` (Windows),
`~/.config/uupdump-client/settings.json` (Linux).

---

## Limites connues et dépannage

- **« Trop de requêtes » (HTTP 429)** : uupdump.net limite le débit par IP.
  L'application réessaie automatiquement ; si ça persiste, attends une minute.
- **« Cette build n'est pas (encore) disponible… »** (`UNSUPPORTED_COMBINATION`) :
  les packs de la build ne sont pas encore générés côté serveur — réessaie plus tard,
  ou utilise le mode *Mises à jour uniquement*.
- **Builds KB sans langue** (mises à jour .NET, serveur…) : aucune langue/édition
  n'existe pour ces builds ; utilise le mode *Mises à jour uniquement*.
- **Conversion sous Windows** : requiert les droits administrateur (DISM) — la fenêtre
  de contrôle de compte utilisateur est normale. Le chemin du projet est passé
  correctement même avec des espaces, mais évite les caractères exotiques par prudence.
- **Conversion sous Linux/macOS** : le convertisseur officiel exige
  `aria2c`, `cabextract`, `wimlib-imagex`, `chntpw` et `genisoimage`/`mkisofs`
  (Debian : `sudo apt-get install aria2 cabextract wimtools chntpw genisoimage`).
  L'app détecte les outils manquants et te le dit clairement.
- **URLs expirées** : les liens de téléchargement Microsoft sont signés et temporaires ;
  si une reprise échoue sur un vieux projet, relance la préparation (nouvelle liste).

---

## Crédits et licence

- [uupdump.net](https://uupdump.net) et son API publique — le projet original.
- [uup-converter](https://git.uupdump.net/uup-dump/converter) (multiplateforme) et le
  convertisseur Windows, utilisés tels quels pour la conversion.
- Ce client est un logiciel indépendant, **non affilié** à uupdump.net ni à Microsoft.
- Code de l'application : MIT.
