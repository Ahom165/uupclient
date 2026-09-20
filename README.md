# UUP dump Client (Rust natif)

Client natif **léger** et **sobre** pour [UUP dump](https://uupdump.net) : recherchez
n'importe quelle build de Windows, configurez le package, ajoutez vos drivers,
et générez l'ISO finale — le tout dans une seule fenêtre.

![GUI](https://img.shields.io/badge/GUI-egui%20%2F%20eframe-blue) ![Lang](https://img.shields.io/badge/lang-Rust%201.85%2B-orange)

## Fonctionnalités

| | |
|---|---|
| **Recherche** | • Base des builds connues (comme la recherche du site) • Dernières builds directes depuis Windows Update (Canary / Dev / Beta / Release Preview / Retail) • Coller un ID d'update (UUID) |
| **Package** | Choix de la langue (noms complets) et des éditions, estimation de la taille du téléchargement |
| **Options** | ISO (wim/esd), intégration des mises à jour (AddUpdates), nettoyage composants (Cleanup), ResetBase, .NET 3.5, éditions virtuelles, suppression d'Edge, SkipWinRE, **auto-clean** des fichiers temporaires |
| **Drivers** | Ajoutez des dossiers de drivers `.inf` — injectés dans l'ISO via le mécanisme officiel `Drivers/ALL` \| `OS` \| `WinPE` du convertisseur UUP dump |
| **Build ISO** | Utilise le **convertisseur officiel** (`uup-converter-wimlib`, `convert-UUP.cmd`) téléchargé et vérifié par SHA-256, lancé automatiquement après le téléchargement |

## Compilation

```bash
cargo build --release
```

Binaire : `target/release/uupdump-client` (Windows : `uupdump-client.exe`).

Dépendances de compilation : uniquement Rust (aucune lib système — egui/eframe
avec rendu glow, HTTP via ureq).

## Utilisation

1. **Builds** — recherchez par numéro (`26100`), mots-clés (`24H2`), ou via WU
   en direct ; cliquez sur une ligne pour la sélectionner. Vous pouvez aussi
   coller directement un UUID d'update.
2. **Package** — choisissez la langue et une ou plusieurs éditions
   (`Professional`, `Home`…). « Estimer la taille » calcule le volume à
   télécharger.
3. **Options & drivers** — dossier de destination, options du média, et vos
   dossiers de drivers (cibles : toutes images / OS / WinPE).
4. **Téléchargement** — lance le job :
   liste de fichiers → téléchargement parallèle (aria2c, vérification SHA-256)
   → extraction du convertisseur officiel → écriture de `ConvertConfig.ini`
   → copie des drivers → `convert-UUP.cmd` (une fenêtre console s'ouvre ;
   validez l'élévation UAC) → ISO créée à la racine du dossier → auto-clean.

## Arborescence du dossier de travail

```
26100_1742_fr-fr_PROFESSIONAL/
├── UUPs/                  # fichiers UUP téléchargés (supprimés si auto-clean)
├── files/                 # 7zr.exe, uup-converter-wimlib.7z, aria2c.exe
├── Drivers/ALL|OS|WinPE/  # vos drivers injectés dans l'ISO
├── ConvertConfig.ini      # options écrites par le client
└── *.iso                  # résultat final
```

## Notes techniques

- **API** : `api.uupdump.net` (JSON) — `fetchupd.php`, `listid.php`,
  `listlangs.php`, `listeditions.php`, `get.php`. Gestion du 429
  `USER_RATE_LIMITED` (l'API limite le débit par IP : patientez quelques
  secondes).
- **Convertisseur** : le client télécharge `uup-converter-wimlib-v126.7z` et
  `7zr.exe` depuis `uupdump.net/misc/` et vérifie leurs empreintes SHA-256
  (valeurs publiées par UUP dump dans `autodl_files/converter_windows`).
- **Drivers** : le convertisseur officiel intègre les `.inf` trouvés dans
  `Drivers/ALL` (install.wim de toutes les éditions), `Drivers/OS`
  (install.wim) et `Drivers/WinPE` (boot.wim).
- **Linux/macOS** : téléchargement identique ; la conversion utilise
  `convert.sh` (nécessite `aria2c`, `cabextract`, `wimlib-imagex`, `chntpw`,
  `genisoimage`). Les builds récentes se convertissent mieux depuis Windows.
- Configuration persistée dans `<config_dir>/uupdump-client/config.json`.

## Limites connues

- L'API ne retourne qu'une édition par téléchargement : sélectionner plusieurs
  éditions revient à prendre la première (les éditions virtuelles du
  convertisseur permettent d'obtenir les autres à partir de Pro/Home).
- La recherche WU directe (`fetchupd.php`) ne retourne parfois rien pour le
  canal Retail selon l'état des serveurs Microsoft — utilisez alors la
  recherche dans les builds connues. `latest` ne s'applique qu'aux canaux
  Insider (Dev, Beta, Canary) ; en Retail, indiquez un numéro précis
  (`19045`, `26100`…).
- Les packages sans langues (updates .NET / arm64) ne sont pas téléchargeables
  via UUP dump : choisissez une « Feature Update ».
- La fenêtre de conversion est ouverte par le convertisseur officiel (élévation
  UAC) ; l'appli détecte l'ISO dès qu'elle apparaît.

## Interface adaptative

Toutes les largeurs sont calculées à partir de l'espace disponible : la barre
de recherche, les listes de canaux et les grilles de résultats restent
entièrement visibles même dans une fenêtre étroite (aucun élément ne sort de
l'écran). Les pages « Package » et « Options » défilent verticalement ; les
titres longs sont abrégés (titre complet au survol).

Variable d'environnement pratique pour les tests : `UUPDUMP_SIZE=700x520`
force la taille de la fenêtre au premier lancement.
