# ManyDesk

macOS və Windows üçün AnyDesk tipli uzaq masaüstü tətbiqi — **serversiz, mərkəzsiz (P2P)**, yalnız **public STUN** serverlərindən istifadə edir.

## İmkanlar

- Ekran paylaşımı (çox monitor dəstəyi, ekran seçimi) — yalnız dəyişən hissələr ötürülür, VP9 ekran-məzmunu rejimi
- Keyfiyyət rejimləri: ⚡ Sürət (az trafik) / ⚖️ Balans / ✨ Keyfiyyət — viewer sessiya zamanı dəyişir
- **Səsli danışıq** (iki tərəfli, exo/küy azaldılması ilə) və **yazılı söhbət** hər sessiyanın içində
- Uzaqdan siçan və klaviatura idarəetməsi (klik, cüt klik, sağ klik, sürükləmə, scroll, Cmd/Shift/Alt/Ctrl qısayolları)
- Mübadilə buferinin (clipboard) iki tərəfli sinxronizasiyası
- İki tərəfli fayl ötürülməsi (düymə və ya uzaq ekrana drag & drop) → `~/Downloads/ManyDesk`
- Host tərəfdə idarəetmə və buferi sessiya zamanı söndürüb-yandırmaq
- 6 rəqəmli təhlükəsizlik kodu (MITM yoxlaması), canlı statistika (marşrut, RTT, fps, bitrate)
- Sessiya zamanı ekranın yuxuya getməsinin qarşısı alınır

## Dil

Azərbaycanca, English, Русский, Türkçe. Defolt — əməliyyat sisteminin dili (macOS, Windows, Linux üçün eyni üsulla: `app.getPreferredSystemLanguages()`); dəstəklənməyən dildə English. Ayarlar → Dil ilə dərhal dəyişir. Diaqnostika jurnalı texniki olduğu üçün ingiliscədir.

## Trafik optimallaşdırması

Ekran video kodeki ilə ötürülür. Kodek yalnız ilk kadrı tam göndərir, sonrakı kadrlarda **ancaq əvvəlki kadrdan fərqlənən bloklar** (hərəkət vektorları + qalıq) ötürülür. Ekran dəyişmirsə, yeni kadr ümumiyyətlə yaranmır.

Ölçülmüş nəticələr (1920×1080 mətnli ekran):

| Vəziyyət | Trafik |
|---|---|
| Sabit ekran | ~0 kbit/s |
| Yazı yazmaq (VP9) | ~1.4 Mbit/s |
| Scroll, "Balans" | ~2.1 Mbit/s |
| Scroll, "⚡ Sürət" (yarım ölçü, 20 fps) | ~0.36 Mbit/s |

- **VP9** üstün tutulur (mətn və UI üçün VP8-dən xeyli səmərəli). Dəstəklənmirsə AV1 → H.264 → VP8.
- Bant genişliyi azalanda WebRTC bitrate-i avtomatik aşağı salır (congestion control).
- Statistika sətrində kodek, cari bitrate və sessiyanın ümumi trafiki (MB) görünür.

## Səs və söhbət

- Səs kanalı ilk kodun içində əvvəlcədən razılaşdırılır, ona görə mikrofonu açmaq/bağlamaq yeni kod mübadiləsi tələb etmir.
- Mikrofon defolt olaraq bağlıdır; 🎤 düyməsi ilə açılır. Qarşı tərəfin mikrofonu açıq olanda 🔊 göstərilir.
- Söhbət 💬 düyməsi ilə açılır; oxunmamış mesaj sayı göstərilir, pəncərə fokusda deyilsə bildiriş gəlir.
- Söhbət yalnız yaddaşda saxlanılır və sessiya bitəndə silinir. Hər şey DTLS/SRTP ilə şifrələnir.

## Arxitektura

```
 Host (ekranı paylaşan)                                   Viewer (idarə edən)
 ┌───────────────────────┐  sorğu → Qəbul et → dəvət → cavab  ┌───────────────────────┐
 │ getDisplayMedia       │ ◀──── Nostr relay-lər + MQTT ─────▶ │ ünvan: 482 913 557    │
 │ RTCPeerConnection     │   (imzalı, şifrəli, bir neçə KB)    │ RTCPeerConnection     │
 │ input helper ◀────────┼──── ekran / input / səs / fayl ─────┼── siçan/klaviatura    │
 └──────────┬────────────┘      (birbaşa P2P, DTLS/SRTP)       └──────────┬────────────┘
            │        yalnız öz public IP-ni öyrənmək üçün                  │
            └────────────▶  public STUN serverləri  ◀──────────────────────┘
                 (lazım olanda: TURN relay — Cloudflare / öz serveriniz)
```

- **Öz serverimiz yoxdur.** Qoşulma mesajları (SDP + ICE namizədləri, `deflate` ilə sıxılmış) public Nostr relay-ləri və MQTT brokerləri ilə ötürülür — aşağıdakı "ID ilə qoşulma" bölməsinə baxın.
- **Public STUN.** 83 public STUN serveri yoxlanıldı (56-sı işləyir); defolt siyahıda müxtəlif provayder, ölkə və portlardan 15-i var: Google (19302, 3478), Cloudflare (3478, **443**), Nextcloud (**443**), Metered (**80**), Twilio, Telnyx, BlackBerry, Framasoft, T-Online, IPFire, Zadarma, Sipnet, Xiaomi. Bir provayderi və ya 19302/3478 portlarını bloklayan şəbəkələr qalanlarına çatır. Ayarlarda dəyişmək olar (yalnız `stun:` ünvanları).
- **Şifrələmə.** Bütün trafik WebRTC DTLS/SRTP ilə uçdan-uca şifrələnir. Təhlükəsizlik kodu hər iki tərəfin DTLS sertifikat barmaq izlərindən (SHA-256) hesablanır; hər iki ekranda eyni rəqəmlər görünürsə, kodlar yolda dəyişdirilməyib.
- **Kanallar:** video track (ekran), `ctl` data channel (input, bufer, icazələr), `file` data channel (64 KB-lıq hissələr, backpressure ilə).
- **Input injeksiyası:** kiçik native helper, Electron onu stdin üzərindən JSON əmrlərlə idarə edir.
  - macOS: `native/InputHelper.swift` — CoreGraphics `CGEvent` (universal: Intel + Apple Silicon)
  - Windows: `native/win/inputhelper_windows.go` — `SendInput` + `SetCursorPos`, scan-code əsaslı klaviatura, per-monitor DPI dəstəyi
- **Mac ↔ Windows:** platformalar fərqlidirsə, viewer Cmd ↔ Ctrl düymələrini avtomatik dəyişir (Mac-dən Cmd+C → Windows-da Ctrl+C və əksinə).

```
src/main/main.js      Electron main: pəncərə, icazələr, ekran mənbəyi, bufer, fayl yazma
src/main/input.js     Normallaşdırılmış koordinatları ekran nöqtələrinə çevirir, helper-i idarə edir
src/main/keymap.js    KeyboardEvent.code → macOS virtual keycode
src/main/keymap-win.js KeyboardEvent.code → Windows scan code
src/renderer/rtc.js   WebRTC sessiyası, kod kodlaşdırma, SAS, fayl ötürmə, statistika
src/renderer/app.js   İnterfeys məntiqi
native/InputHelper.swift
native/win/inputhelper_windows.go
```

## İşə salma

Tələblər: Node.js 18+; macOS helper üçün Xcode Command Line Tools (`swiftc`), Windows helper üçün Go 1.20+.

```bash
npm install
```

```bash
npm start
```

Windows-da (Go lazımdır, Git Bash ilə):

```bash
npm run start:win
```

Paketləmə (hamısı Mac-dən qurula bilir):

```bash
npm run dist
```

```bash
npm run dist:win
```

| Fayl | Platforma |
|---|---|
| `ManyDesk-<v>-arm64.dmg` / `.zip` | Mac, Apple Silicon (M1/M2/M3…) |
| `ManyDesk-<v>-x64.dmg` / `.zip` | Mac, Intel |
| `ManyDesk-<v>-win-x64.exe` | Windows 10/11 installer |
| `ManyDesk-<v>-win-x64-portable.exe` | Windows, quraşdırmadan işləyən |

Minimum: macOS 13, Windows 10 (64-bit).

## Paylaşma və "damaged" xətası

Tətbiq Apple Developer ID ilə imzalanmadığı üçün internetdən (WhatsApp, Telegram, brauzer) yüklənəndə macOS onu karantinə qoyur. Bu zaman "ManyDesk.app is damaged" və ya "Apple could not verify…" xətası çıxa bilər. Həll yolu — tətbiqi `/Applications`-a köçürdükdən sonra:

```bash
xattr -dr com.apple.quarantine /Applications/ManyDesk.app
```

Və ya System Settings → Privacy & Security → **Open Anyway**. Bu xəbərdarlığın tam aradan qalxması üçün ödənişli Apple Developer hesabı ilə imzalayıb notarizasiya etmək lazımdır.

## İstifadə

İnterfeys AnyDesk-in iş axınını izləyir (öz ikonları və brendi ilə):

- **Sol panel → "Bu masa":** bu kompüterin 9 rəqəmli ünvanı (məs. `482 913 557`) və relay statusu.
- **Yuxarıda "Uzaq ünvanı daxil edin":** qarşı tərəfin ünvanını yazıb → (Qoşul).
- **Sessiya tabı:** qoşulma başlayan kimi açılır ("Qəbul etməsi gözlənilir…"), qoşulduqdan sonra uzaq ekran + alət paneli (söhbət, mikrofon, fayl, keyfiyyət, tam ekran, ayrıl).
- **Qəbul pəncərəsi (host):** "… ekranınıza qoşulmaq istəyir", icazə düymələri (klaviatura/siçan, bufer) → **Qəbul et / Rədd et**. Eyni anda gələn sorğular növbəyə düşür və bir-bir göstərilir.
- **Host çox-sessiyalıdır:** eyni anda 5 nəfərə qədər qoşula bilər. Hər qoşulan üçün sağ altda ayrıca kart: öz təhlükəsizlik kodu, öz icazələri (klaviatura/siçan, bufer), statistika, söhbət, mikrofon, fayl, **Ayır**. Kartı kiçiltmək olar.
- **Ana səhifə → "Son sessiyalar":** əvvəlki bağlantıların şəkilli plitələri; klik → yenidən qoşulma. Hər plitədə canlı status: 🟢 **Onlayn**, 🟠 **Məşğul** (o kompüter maksimum sayda qoşulanı qəbul edib), ⚪ **Oflayn**.
- **Söhbət** ayrıca sütunda açılır — uzaq ekran, alət paneli və bağlantı məlumatı onun altında qalmır.
- **Bir neçə kompüterə eyni anda qoşulmaq:** hər yeni ünvan ayrıca tabda açılır (tabda ad + ünvanın son 3 rəqəmi, oxunmamış mesaj sayı, vəziyyət nöqtəsi).
- **Tam ekran (AnyDesk kimi):** alət panelindəki düymə və ya **Ctrl+Alt+Shift+F** — pəncərə bütün monitoru tutur, yalnız uzaq ekran qalır; alət paneli siçan ekranın yuxarı kənarında dayananda sürüşüb açılır. Eyni qısayol normal rejimə qaytarır. Esc uzaq kompüterə gedir (tam ekrandan çıxarmır).

#### Onlayn status necə işləyir

Server olmadan: hər 45 saniyədən bir siyahıdakı ünvanlara ID kanalları (Nostr + MQTT) ilə kiçik şifrəli "ping" gedir; həmin kompüterdə ManyDesk açıqdırsa, "pong" (ad + məşğulluq) ilə cavab verir. Cavab 100 saniyədən köhnədirsə — oflayn. Ping-i yalnız ünvanı bilən göndərə bilər, cavablar sürət baxımından məhdudlaşdırılıb. "Ekran paylaşımı → Ünvanımı bilənlər onlayn olub-olmadığımı görə bilsin" söndürülsə, kompüter həmişə oflayn görünür (qoşulma sorğuları yenə gəlir).

## Sessiyaların izolyasiyası

Hər sessiya tamamilə ayrıdır — öz WebRTC bağlantısı, öz şifrələmə açarları və təhlükəsizlik kodu, öz video/audio elementi, öz söhbəti və statistikası:

| | Aktiv (görünən) tab | Fon tabları |
|---|---|---|
| Görüntü | göstərilir | arxada qəbul olunmağa davam edir |
| Uzaq səs | eşidilir | **susdurulur** |
| Sizin mikrofonunuz | yalnız bu sessiyaya gedir | **trek söndürülür**, qarşı tərəfə "mikrofon bağlı" bildirilir; taba qayıdanda bərpa olunur |
| Klaviatura / siçan | yalnız bu sessiyaya | heç nə getmir; tab dəyişəndə basılı düymələr buraxılır |
| Mübadilə buferi | yalnız bu sessiya ilə | nə oxuya, nə yaza bilər |
| Söhbət | öz tarixçəsi | öz tarixçəsi + oxunmamış sayı tabda; mesajlar heç vaxt başqa sessiyaya düşmür |
| Fayl | yalnız bu sessiyaya | — |

Host tərəfində də hər qoşulan ayrıdır: hər birinə ayrıca ekran axını (birinin "Sürət" seçimi digərinin keyfiyyətinə təsir etmir), ayrıca söhbət (A-nın mesajı B-yə getmir), mikrofon yalnız açıldığı istifadəçiyə gedir, icazələr hər istifadəçi üçün ayrıca seçilir. Bir viewer-in göndərdiyi bufer mətni digər viewer-lərə avtomatik ötürülmür. Klaviatura/siçan icazəsi olan bir neçə viewer eyni siçanı idarə edir — buna görə icazəni yalnız ehtiyac olana verin.

**Kim nə edir:** hostun ekranında hər viewer-in hərəkəti rəngli etiketlə göstərilir — adı, ünvanı və əməliyyatı ("klikləyir", "yazır", "sürüşdürür"); klikdə dalğa effekti. Etiketin rəngi sağ altdakı kartın rəngi ilə eynidir. Etiketlər şəffaf, klikləri ötürən overlay pəncərəsindədir və **ekran tutulmasından çıxarılıb** (macOS/Windows content protection) — onları yalnız host görür, viewer-lər bir-birini görmür.

Uzaq ekran həmişə **tam** ötürülür (kəsilmədən, orijinal nisbətdə) və viewer pəncərəsinin ölçüsündən asılı olmayaraq **tam görünəcək şəkildə miqyaslanır** (object-fit: contain); siçan koordinatları bu miqyasa uyğun hesablanır. "Sürət" rejimi ölçünü yarıya endirir, amma kəsmir.

## ID ilə qoşulma necə işləyir

Öz serverimiz yoxdur. Qısa qoşulma mesajları (bir neçə KB) **iki müstəqil public şəbəkə** ilə paralel ötürülür:

- **11 Nostr relay** (müxtəlif ölkələrdə müstəqil operatorlar)
- **4 MQTT broker** (HiveMQ, EMQX, EMQX-CN, Mosquitto — WebSocket üzərindən)

Şəbəkə birini bloklasa (məs. Nostr domenləri), digəri işləyir — hər iki ssenari real yoxlanılıb. Görüntü, idarəetmə, səs, fayllar onlardan keçmir — birbaşa P2P gedir.

**Saat fərqi:** kompüterlərin saatı dəqiqələrlə fərqli ola bilər (xüsusən Windows) və relay-lər belə mesajları rədd edir. Tətbiq dəqiq vaxtı şəbəkədən (Cloudflare) öyrənir və mesajları onunla imzalayır — sistem saatı 25 dəqiqə səhv olanda da 11/11 relay qəbul edir.

```
viewer ──sorğu (ID-dən törədilən açarla şifrəli)──▶ relay-lər ──▶ host: Qəbul et?
viewer ◀──dəvət kodu (təsadüfi sessiya açarı)────── relay-lər ◀── host
viewer ──cavab kodu─────────────────────────────▶ relay-lər ──▶ host  →  birbaşa P2P
```

- Bütün relay və brokerlərə paralel göndərilir; birinin çatdırması kifayətdir. Nostr mesajları "ephemeral"-dır, MQTT QoS 0-dır (saxlanmır). Relay-in yenə də saxladığı köhnə sorğular (yenidən başladıqdan sonra) EOSE ilə ayırd edilib atılır.
- Hər mesaj Schnorr (secp256k1) ilə imzalanır və tətbiq imzanı özü yoxlayır — relay mesajı saxtalaşdıra bilməz.
- ID yalnız hostu tapmaq və ilk sorğunu şifrələmək üçündür. Sonrakı mesajlar təsadüfi sessiya açarı ilə şifrələnir, hər tərəf digərinin imza açarını "pin" edir: ID-ni bilən başqa biri cavabı oxuya və ya araya cavab sala bilməz.
- Host hər sorğunu əl ilə qəbul etməlidir; 60 saniyə cavab verilməsə, avtomatik rədd edilir.
- ID kompüterdə saxlanılır və dəyişmir; "Yeni ID" ilə yenilənə bilər (köhnəsi işləməz).

## Windows qeydləri

- Xüsusi icazə lazım deyil. İlk açılışda Windows Firewall şəbəkə icazəsi soruşa bilər — "Private networks" üçün icazə verin.
- Installer imzasızdır, ona görə SmartScreen "Windows protected your PC" göstərə bilər: **More info → Run anyway**.
- Windows UAC pəncərələri və administrator kimi işləyən proqramlar adi istifadəçi prosesindən idarə edilə bilmir (Windows təhlükəsizlik məhdudiyyəti). Bunlar üçün ManyDesk-i "Run as administrator" ilə açın.

## macOS icazələri (host tərəfdə)

- **Ekran yazılışı** (System Settings → Privacy & Security → Screen Recording)
- **Əlçatanlıq / Accessibility** — uzaqdan idarəetmə üçün

`npm start` ilə işlədəndə icazələr "Electron" tətbiqinə verilir; paketlənmiş versiyada "ManyDesk"-ə. İcazə verdikdən sonra tətbiqi yenidən başladın.

## Mobil internet və TURN relay

Mobil operatorlar (Azercell, Bakcell, Nar və s.) CGNAT / simmetrik NAT işlədir: hər yeni ünvan üçün təsadüfi port verilir, ona görə yalnız STUN ilə birbaşa qoşulma alınmır. Həll — TURN relay:

**Tövsiyə — Cloudflare Realtime TURN (ayda 1000 GB pulsuz):**

1. [dash.cloudflare.com](https://dash.cloudflare.com)-da pulsuz hesab açın.
2. **Realtime** (əvvəlki adı *Calls*) → **TURN Server** → **Create** → *Turn Token ID* və *API Token*-i kopyalayın.
3. ManyDesk → Ayarlar → **TURN relay** → "Cloudflare" → açarları yapışdırın → **Yoxla** → **Yadda saxla**.

Tətbiq bu açarla hər sessiyadan əvvəl 24 saatlıq müvəqqəti giriş məlumatı alır (`rtc.live.cloudflare.com` API) — öz serverə ehtiyac yoxdur. **Yalnız bir tərəfdə** ayarlamaq kifayətdir.

**Alternativ:** "Digər TURN (əl ilə)" — [Metered Open Relay](https://www.metered.ca/tools/openrelay/) (ayda 20 GB), ExpressTURN və ya öz coturn serveriniz.

Qeydiyyatsız işləyən açıq TURN serverləri artıq qalmayıb: 2026-cı ilin oktyabrında 20-dən çox məşhur ünvan yoxlanıldı (openrelay, freestun, freeturn, PeerJS-in daxili TURN-u və s.) — hamısı bağlanıb və ya DNS-dən silinib.

- Relay yalnız birbaşa yol (LAN/STUN) tapılmayanda istifadə olunur — ICE həmişə birbaşa yolu üstün tutur.
- Trafik relay-dən keçsə də DTLS/SRTP ilə uçdan-uca şifrəli qalır; relay məzmunu görmür.
- Parol yalnız həmin kompüterdə saxlanılır; dəvət/cavab koduna yalnız relay-in ünvanı düşür.
- Relay üzərindən qoşulanda viewer avtomatik "⚡ Sürət" rejiminə keçir (pulsuz paketin limitinə qənaət).

## Məhdudiyyətlər

- **Simmetrik NAT (mobil internet):** TURN ayarlanmayıbsa, birbaşa qoşulma alınmaya bilər — yuxarıdakı bölməyə baxın.
- **Nəzarətsiz giriş (unattended access) yoxdur** — host hər sorğunu "Qəbul et" ilə təsdiqləməlidir, tətbiq açıq olmalıdır.
- Kompüterin öz səsi (musiqi, video səsi) ötürülmür — yalnız mikrofon.
- Paket yalnız ad-hoc imzalıdır (Apple Developer ID və notarizasiya yoxdur). Ətraflı: aşağıdakı "Paylaşma" bölməsi.
