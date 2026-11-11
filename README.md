# ManyDesk

AnyDesk tipli uzaq masaüstü tətbiqi — **öz serveri olmadan, birbaşa P2P**. Rust ilə yazılmış tək native tətbiqdir (egui interfeysi, öz QUIC protokolu). Kompüterlər bir-birini public Nostr relay-ləri, MQTT brokerləri və ya yerli şəbəkə ilə tapır. Görüntü, idarəetmə, səs və fayllar birbaşa iki kompüter arasında gedir.

| Platforma | Paket | Qeyd |
|---|---|---|
| macOS 13+ | `ManyDesk-<v>-mac-universal.dmg` | Apple Silicon + Intel |
| Windows 10/11 (64-bit) | `ManyDesk-<v>-win-x64.exe` | tək fayl, quraşdırma tələb etmir |
| Linux (amd64, arm64) | `.deb`, `.tar.gz` | X11 və Wayland |
| Android 8.0+ | `ManyDesk-<v>-android.apk` | həm idarə edir, həm ekranını paylaşır |
| iOS | mənbədən (`scripts/build-ios.sh`) | yalnız idarə edir (iOS ekran paylaşımına icazə vermir) |

## İmkanlar

**Sessiya növləri** (AnyDesk-in "File Transfer"-i və RustDesk-in terminalı kimi)
- **Masaüstü:** uzaq ekranı görmək və idarə etmək.
- **Fayl ötürmə:** masaüstünü göstərmədən iki panelli fayl meneceri.
- **Terminal:** masaüstünü göstərmədən uzaq kompüterdə shell (macOS/Linux-da login shell, Windows-da PowerShell).

Ünvan sətrindəki fayl/terminal düymələri, son sessiya kartlarının ⋯ menyusu və ya sağ klik (telefonda uzun basma) ilə açılır. Fayl və terminal sessiyalarında host ekranı tutmur və yalnız həmin rejimin icazəsini verir.

**Ekran və idarəetmə**
- H.264 video: yalnız dəyişən hissələr göndərilir, sabit ekran demək olar ki, trafik tutmur. Keyfiyyət: Avtomatik / Sürət / Balans / Keyfiyyət.
- Çox monitor (Ctrl+Alt+Shift+←/→), görünüş: pəncərəyə sığdır / 1:1 / dartılmış, tam ekran (Ctrl+Alt+Shift+F).
- Siçan, klaviatura (düymələr və ya "mətn" rejimi), Mac ↔ Windows arasında Cmd ↔ Ctrl avtomatik dəyişir.
- Uzaq ekranın ölçüsünü dəyişmək (sessiya bitəndə geri qayıdır), macOS-da **virtual ekran** əlavə etmək (monitoru olmayan Mac üçün).
- Telefonda touchpad və birbaşa toxunma rejimləri.

**Səs, kamera, söhbət**
- İki tərəfli səs (Opus) və hər iki tərəfin kamerası (kiçik pəncərədə).
- Hər sessiyanın öz yazılı söhbəti.

**Fayllar və klipbord**
- Fayl meneceri (yükləmə/endirmə, qovluq yaratmaq, ad dəyişmək, silmək) və drag & drop.
- İki tərəfli klipbord: mətn, formatlı mətn (HTML), şəkil, fayl və qovluqlar. Kopyalanan fayllar AnyDesk kimi sessiyada Ctrl/Cmd+V basılanda göndərilir və uzaq tərəfdə özü yapışdırılır.

**Sessiya hərəkətləri** (⋯ menyusu)
- Ctrl+Alt+Del / Force Quit, kilid, Cmd/Alt+Tab və digər xüsusi düymələr.
- Uzaq kompüteri yenidən başlatmaq və avtomatik yenidən qoşulmaq.
- Məxfilik rejimi (hostun ekranı gizlədilir, yerli siçan və klaviatura bloklanır), sessiya bitəndə kilidləmək.
- Lövhə (uzaq ekranda çəkmək), ekran görüntüsü, tərəfləri dəyişmək.
- TCP tunel: yerli port → uzaq kompüterdən `host:port`.

**Nəzarətsiz giriş**
- Parol (kompüterdə yalnız PBKDF2 açarı saxlanılır), istəyə görə TOTP ikinci faktoru.
- "Bu cihaza etibar et": TOTP kodu bir dəfə daxil edildikdən sonra o cihazdan bir daha soruşulmur. Host etibarlı cihazları silə bilər.
- Windows xidməti: kimsə daxil olmadıqda da (giriş ekranında) kompüterə qoşulmaq olur.

**Tapmaq və qoşulmaq**
- 9 rəqəmli ID. Ünvan sətrində son sessiyalardan və yerli şəbəkədən avtomatik tamamlama (ID, ad və ya IP ilə).
- Son sessiyalar kartlarında canlı status: onlayn / məşğul / oflayn.
- **Yerli şəbəkə:** eyni şəbəkədəki ManyDesk-lər avtomatik görünür, internet olmadan da qoşulmaq olur. ID əvəzinə IP yazmaq olar (birbaşa IP). Sönülü kompüteri **Wake-on-LAN** ilə yandırmaq olar.
- Host eyni anda 5 viewer qəbul edir. Hər viewer-in öz kartı, icazələri və rəngli siçan etiketi var.

## Arxitektura

```
 Host                                                         Viewer
 ┌─────────────────────┐  sorğu → Qəbul et → təklif → cavab   ┌─────────────────────┐
 │ ekran tutma         │ ◀── Nostr + MQTT + yerli şəbəkə ───▶ │ ID: 482 913 557     │
 │ H.264 kodlayıcı     │     (imzalı, şifrəli, bir neçə KB)   │ H.264 dekoder       │
 │ input injeksiyası ◀─┼── ekran / input / səs / fayl ────────┼── siçan, klaviatura │
 └──────────┬──────────┘   (birbaşa QUIC, TLS 1.3, P2P)        └──────────┬──────────┘
            │          yalnız öz public ünvanını öyrənmək üçün            │
            └──────────▶  public STUN serverləri  ◀───────────────────────┘
                (lazım olanda TURN relay: Cloudflare və ya öz serveriniz)
```

- **Görüş nöqtəsi (rendezvous):** qısa qoşulma mesajları **11 Nostr relay**, **4 MQTT broker** və **yerli şəbəkə** (UDP broadcast, port 21140) ilə paralel ötürülür. Hər mesaj Schnorr (secp256k1) ilə imzalanır və tətbiq imzanı özü yoxlayır. İlk sorğu ID-dən törədilən açarla, sonrakılar təsadüfi sessiya açarı ilə şifrələnir. Dəqiq vaxt şəbəkədən (Cloudflare) öyrənilir, ona görə səhv sistem saatı mane olmur.
- **MDP:** ManyDesk-in öz protokolu. UDP deşik açma (hole punching) ilə alınmış birbaşa yol üzərində QUIC (TLS 1.3, BBR) işləyir. Hər video kadr ayrıca axında gedir: itən paket yalnız həmin kadrı gecikdirir. Səs datagramlarla, idarəetmə və fayllar ayrı axınlarla ötürülür.
- **Video:**

  | | Tutma | Kodlama | Dekodlama |
  |---|---|---|---|
  | macOS | ScreenCaptureKit | VideoToolbox (aparat) | VideoToolbox |
  | Windows | DXGI | Media Foundation (Quick Sync / NVENC / AMF), yoxdursa OpenH264 | Media Foundation (DXVA) |
  | Linux | X11 MIT-SHM / Wayland portal | OpenH264 | VA-API (GStreamer) |
  | Android | MediaProjection | OpenH264 | OpenH264 |

- **Input:** macOS-da kiçik Swift köməkçisi (`native/InputHelper.swift`, CoreGraphics), Windows-da Go köməkçisi (`native/win/`, `SendInput`, exe-nin içindədir), Linux-da XTest, Android-də AccessibilityService.

## Təhlükəsizlik

- Sessiyanın bütün trafiki uçdan-uca şifrələnir. Hər tərəf öz sertifikatını yaradır, barmaq izi şifrəli görüş mesajı ilə ötürülür və QUIC/TLS yalnız həmin sertifikatı qəbul edir (hər iki istiqamətdə pin).
- 6 rəqəmli təhlükəsizlik kodu hər iki sertifikatdan hesablanır. Hər iki ekranda eyni rəqəmlər görünürsə, araya kimsə girməyib.
- Nəzarətsiz giriş parolu şəbəkədə getmir: viewer `HMAC(PBKDF2(parol), nonce | host | viewer | TOTP)` ilə sübut göndərir, 3 səhv cəhddən sonra bağlantı kəsilir.
- TURN relay trafiki görsə də, məzmunu oxuya bilmir.
- Host hər sorğunu "Qəbul et" ilə təsdiqləyir (və ya nəzarətsiz giriş parolu). Hər viewer-in icazələri ayrıca seçilir və sessiya zamanı dəyişdirilə bilər. İstəyə görə yalnız siyahıdakı ID-lərə icazə vermək olar.

## Layihənin quruluşu

```
app/                  Rust tətbiqi (bütün platformalar)
  src/ui.rs, ui/      interfeys (egui): ana ekran, sessiya, fayl meneceri, terminal, ayarlar
  src/core.rs, core/  sessiyalar, sorğular, icazələr, klipbord, ekran rejimləri, terminal, tunellər
  src/mdp/            QUIC protokolu, NAT keçidi, TLS pin, TURN müştərisi
  src/rendezvous.rs   Nostr + MQTT görüş nöqtəsi
  src/lan.rs          yerli şəbəkə: kəşf, birbaşa IP, Wake-on-LAN
  src/media/          ekran tutma, H.264 kodlama/dekodlama, kamera
  src/platform/       icazələr, Windows xidməti, ekran rejimləri, virtual ekran
  src/android.rs, ios.rs  telefon körpüləri
  assets/i18n.json    tərcümələr (az, en, ru, tr)
android/              Android qabığı (Java: ekran tutma, AccessibilityService, bildirişlər)
ios/                  iOS qabığı
native/               macOS və Windows input köməkçiləri
scripts/              build skriptləri
docker/               Linux build mühiti
```

## Build

Hamısı Mac-dən qurulur.

Tələblər:
- Rust (rustup)
- Mac üçün: Xcode Command Line Tools
- Windows üçün: `mingw-w64`, `cmake`, Go (input köməkçisi)
- Linux üçün: Docker
- Android üçün: `scripts/setup-android.sh` hər şeyi quraşdırır
- iOS üçün: tam Xcode

Mac, Windows və Linux paketləri (`dist-native/`-ə):

```bash
scripts/package-native.sh mac win
```

```bash
scripts/package-native.sh --bump minor mac win linux
```

`--bump major|minor|patch` versiyanı `app/Cargo.toml`-da artırır. Qayda: yeni funksiya `minor`, düzəliş `patch`. Hər build-ə tarix əsaslı build nömrəsi də yazılır (`manydesk --version`).

Android və iOS:

```bash
scripts/build-android.sh
```

```bash
scripts/build-ios.sh
```

İnkişaf zamanı birbaşa işə salmaq:

```bash
cd app && cargo run --release
```

macOS imzası: Ekran yazılışı və Əlçatanlıq icazələri tətbiqin imzasına bağlıdır. `package-native.sh` ilk dəfə keychain-də "ManyDesk Local Signing" sertifikatı yaradır və bütün build-ləri onunla imzalayır, ona görə yenidən build-dən sonra icazələr itmir. Developer ID varsa: `MANYDESK_SIGN_ID="Developer ID Application: …"`.

### Test üçün mühit dəyişənləri

Eyni Mac-də bir neçə nüsxəni ayrı ayarlarla işə salmaq olar (`HOME=<qovluq>`, Windows-da `MANYDESK_CONFIG_DIR=<qovluq>`). Avtomatik testlər üçün:

| Dəyişən | Nə edir |
|---|---|
| `MANYDESK_AUTOCONNECT=<ID>` | açılan kimi qoşulur (`MANYDESK_AUTOMODE=files\|terminal` ilə rejim) |
| `MANYDESK_AUTOACCEPT=1`, `MANYDESK_AUTOFULL=1` | sorğuları qəbul edir (bütün icazələrlə) |
| `MANYDESK_TESTNOINJECT=1` | uzaq input-u icra etmir, log-a yazır |
| `MANYDESK_SYNTH=1` | ekran əvəzinə sintetik test şəkli |
| `MANYDESK_TESTNORELAY=1` | relay-lər olmadan, yalnız yerli şəbəkə |
| `MANYDESK_TESTPASTE`, `MANYDESK_TESTPASTEFILES`, `MANYDESK_TESTTERM`, `MANYDESK_TESTTYPE`, `MANYDESK_TESTCLICK` | yapışdırma, terminal, yazma və kliki təqlid edir |
| `MANYDESK_DEBUG=1` | ətraflı log |

## Platforma qeydləri

**macOS**
- Host üçün icazələr: **Ekran yazılışı**, **Əlçatanlıq** (idarəetmə üçün), **Yerli şəbəkə** (LAN kəşfi və Wake-on-LAN üçün).
- Paket Apple Developer ID ilə notarizasiya olunmayıb. İnternetdən yüklənəndə "damaged" xətası çıxarsa, `/Applications`-a köçürdükdən sonra:

  ```bash
  xattr -dr com.apple.quarantine /Applications/ManyDesk.app
  ```

**Windows**
- İlk açılışda Firewall şəbəkə icazəsi soruşa bilər: "Private networks" üçün icazə verin.
- Fayl imzasızdır, SmartScreen çıxarsa: **More info → Run anyway**.
- Kimsə daxil olmadıqda da qoşulmaq üçün Ayarlarda Windows xidmətini quraşdırın (administrator icazəsi lazımdır). Ctrl+Alt+Del də xidmət vasitəsilə göndərilir.

**Android**
- İdarə edilmək üçün ManyDesk-in Əlçatanlıq xidmətini yandırın. Android 13+ APK-dan quraşdırılanda bəzi telefonlarda (Samsung, Xiaomi) əvvəlcə Tətbiq məlumatı → ⋮ → "Məhdud ayarlara icazə ver" lazımdır.

**Linux**
- Wayland-də ekran desktop portalı ilə tutulur (ilk dəfə sistem soruşur).

## Mobil internet və TURN relay

Mobil operatorlar (Azercell, Bakcell, Nar və s.) CGNAT / simmetrik NAT işlədir, ona görə yalnız STUN ilə birbaşa yol tapılmaya bilər. Həll TURN relay-dir.

**Tövsiyə: Cloudflare Realtime TURN** (ayda 1000 GB pulsuz):
1. [dash.cloudflare.com](https://dash.cloudflare.com)-da pulsuz hesab açın.
2. **Realtime → TURN Server → Create**, sonra *Turn Token ID* və *API Token*-i kopyalayın.
3. ManyDesk → Ayarlar → **TURN relay** → "Cloudflare" → açarları yapışdırın → **Yoxla** → **Yadda saxla**.

Tətbiq bu açarla müvəqqəti (24 saatlıq) giriş məlumatı alır. Yalnız bir tərəfdə ayarlamaq kifayətdir. Alternativ: "Digər TURN (əl ilə)", yəni öz coturn serveriniz və ya başqa provayder.

Relay yalnız birbaşa yol tapılmayanda istifadə olunur. Trafik relay-dən keçsə də uçdan-uca şifrəli qalır.

## Məhdudiyyətlər

- Paketlər Apple Developer ID / Windows kod imzası ilə imzalanmayıb.
- iOS yalnız viewer-dir: iOS başqa tətbiqlərə ekranı tutmağa icazə vermir.
- Virtual ekran yalnız macOS-dadır (Windows-da imzalı drayver lazımdır).
- Wake-on-LAN yalnız eyni şəbəkədən işləyir və kompüterin şəbəkə kartı bir dəfə yerli şəbəkədə görünməlidir.
- Avtomatik yeniləmə hazırdır, amma manifest ünvanı (`platform/update.rs`) hələ təyin olunmayıb.
