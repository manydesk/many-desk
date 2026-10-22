// UI translations. t('key', { vars }) returns the text in the current language
// (falls back to English, then to the key). Static markup uses data-i18n="key",
// data-i18n-html (trusted markup), data-i18n-title / -placeholder / -aria.
(() => {
  const LANGS = { az: 'Azərbaycanca', en: 'English', ru: 'Русский', tr: 'Türkçe' };

  const D = {
    az: {
      'home': 'Ana səhifə', 'desk.title': 'Bu masa', 'desk.sub': 'Sizin ünvanınız',
      'rv.ready': 'Hazır · {n}/{total} kanal', 'rv.down': 'Kanal yoxdur — internet/firewall', 'rv.connecting': 'Qoşulur…',
      'nav.remote': 'Uzaq masa', 'nav.share': 'Ekran paylaşımı', 'nav.settings': 'Ayarlar', 'foot.secure': 'Uçdan-uca şifrəli · P2P',
      'addr.ph': 'Uzaq ünvan', 'tt.connect': 'Qoşul', 'tt.copy': 'Kopyala', 'tt.settings': 'Ayarlar',
      'perm.banner': 'İdarəetmə üçün macOS icazələri lazımdır.', 'perm.fix': 'İcazə ver',
      'recent.title': 'Son sessiyalar', 'recent.empty': 'Sessiya yoxdur', 'recent.emptyHint': 'Yuxarıda ünvan yazıb qoşulun.',
      'perm.title': 'İcazələr', 'perm.screen': 'Ekran yazılışı', 'perm.screenHint': 'Ekranı göstərmək üçün',
      'perm.open': 'Ayarları aç', 'perm.ax': 'Əlçatanlıq', 'perm.axHint': 'Siçan və klaviatura üçün', 'perm.grant': 'İcazə ver',
      'share.screen': 'Paylaşılan ekran', 'share.defaults': 'Defolt icazələr', 'perm.control': 'Klaviatura və siçan', 'perm.clipboard': 'Bufer',
      'share.addr': 'Ünvan', 'share.presence': 'Onlayn statusumu göstər', 'share.newId': 'Yeni ünvan', 'share.newIdHint': 'Köhnə ünvan işləməyəcək.',
      'share.noScreen': 'Ekran tapılmadı — "Ekran yazılışı" icazəsi verin.',
      'set.lang': 'Dil', 'lang.system': 'Sistem dili',
      'diag.title': 'Diaqnostika', 'diag.hint': 'Qoşulma alınmırsa, hər iki tərəfdə yoxlayın.', 'diag.run': 'Şəbəkəni yoxla', 'diag.copy': 'Jurnalı kopyala',
      'diag.running': 'Yoxlanılır…', 'diag.error': 'Xəta: {e}', 'diag.noLog': 'Hələ cəhd yoxdur', 'diag.copied': 'Jurnal kopyalandı',
      'turn.title': 'TURN relay', 'turn.hint': 'Mobil internet üçün. Yalnız birbaşa yol olmayanda işləyir; bir tərəfdə kifayətdir.',
      'turn.enable': 'TURN istifadə et', 'turn.cf': 'Cloudflare · 1000 GB/ay pulsuz', 'turn.manual': 'Digər TURN',
      'turn.cfSteps': '<b>dash.cloudflare.com</b> → Realtime → TURN Server → Create. Token ID və API Token-i bura yapışdırın.',
      'turn.keyId': 'Token ID', 'turn.token': 'API Token', 'turn.urls': 'Ünvanlar (sətirbəsətir)', 'turn.user': 'İstifadəçi', 'turn.pass': 'Parol',
      'turn.needCf': 'Token ID və API Token daxil edin', 'turn.needUrl': 'turn: ünvanı daxil edin', 'turn.savedErr': 'Saxlanıldı, amma: {e}',
      'turn.saved': 'Saxlanıldı', 'turn.off': 'TURN söndürüldü', 'turn.works': 'İşləyir — "Yadda saxla" basın.', 'turn.none': 'İşləmir — məlumatları yoxlayın.',
      'turn.noResp': 'cavab yoxdur', 'turn.notReady': 'TURN hazır deyil: {e}',
      'stun.title': 'STUN serverləri', 'stun.hint': 'Yalnız stun: ünvanları.', 'stun.saved': 'Saxlanıldı', 'stun.reset': 'Defolt bərpa olundu',
      'btn.save': 'Yadda saxla', 'btn.test': 'Yoxla', 'btn.default': 'Defolt', 'btn.cancel': 'Ləğv et', 'btn.close': 'Bağla',
      'q.auto': 'Avto', 'conn.fallback': 'Alternativ yolla qoşulur…', 'q.speed': 'Sürət', 'q.balanced': 'Balans', 'q.quality': 'Keyfiyyət',
      'tt.chat': 'Söhbət', 'tt.mic': 'Mikrofon', 'tt.micOn': 'Mikrofon açıq', 'tt.file': 'Fayl göndər', 'tt.quality': 'Keyfiyyət / trafik',
      'tt.fs': 'Tam ekran (Ctrl+Alt+Shift+F)', 'tt.fsExit': 'Normal rejim (Ctrl+Alt+Shift+F)', 'tt.remoteMic': 'Qarşı tərəfin mikrofonu açıq',
      'tt.disconnect': 'Ayrıl', 'tt.closeTab': 'Bağla', 'tt.paused': 'Fonda: mikrofon və səs dayanıb', 'tt.collapse': 'Kiçilt', 'tt.remove': 'Sil', 'tt.sas': 'Təhlükəsizlik kodu',
      'sess.click': 'İdarə üçün klikləyin', 'sess.viewOnly': 'Yalnız baxış', 'sess.drop': 'Faylı buraxın', 'sas': 'Təhlükəsizlik kodu:',
      'perm.active': 'İdarəetmə', 'perm.view': 'Baxış',
      'chat.title': 'Söhbət', 'chat.send': 'Göndər', 'chat.ph': 'Mesaj…', 'chat.empty': 'Mesaj yoxdur',
      'acc.wants': 'qoşulmaq istəyir', 'acc.accept': 'Qəbul et', 'acc.reject': 'Rədd et', 'acc.left': '{s} san', 'acc.live': '{n} qoşulub', 'acc.queue': 'növbədə {n}',
      'host.connecting': 'qoşulur…', 'host.connected': 'qoşulub', 'host.remoteMic': 'Mikrofonu açıqdır', 'host.disconnect': 'Ayır', 'host.unknown': 'Naməlum',
      'host.joined': '{who} qoşuldu', 'notif.request': 'Qoşulma sorğusu', 'notif.requestBody': '{who} qoşulmaq istəyir', 'req.cancelled': 'Sorğu ləğv edildi',
      'file.reveal': 'Göstər', 'file.sending': '{who}: göndərilir {f}', 'file.sent': '{who}: göndərildi {f}', 'file.failed': '{who}: alınmadı {f} — {e}',
      'file.receiving': '{who}: qəbul {f}', 'file.received': '{who}: alındı {f}', 'file.noChannel': 'fayl kanalı bağlıdır', 'file.closed': 'sessiya bağlandı',
      'st.relay': 'TURN relay', 'st.lan': 'LAN', 'st.total': 'cəmi {mb} MB',
      'warn.noStun': 'STUN əlçatmazdır — UDP bloklanıb, yalnız LAN işləyəcək.',
      'mic.noPerm': 'mikrofon icazəsi yoxdur', 'mic.failed': 'Mikrofon: {e}', 'mic.noAudio': 'səs kanalı yoxdur',
      'err.noScreen': 'ekran icazəsi yoxdur', 'err.noAnswer': 'Cavab gəlmədi', 'err.relayRejected': 'kanallar qəbul etmədi', 'err.badAnswer': 'Cavab xətalı: {e}',
      'err.answerNotSent': 'cavab göndərilmədi', 'host.failed': '{who}: alınmadı — {e}',
      'peer.ended': 'Qarşı tərəf bitirdi', 'peer.left': 'Qarşı tərəf ayrıldı', 'sess.ended': 'sessiya bitdi', 'conn.lost': 'Bağlantı kəsildi',
      'conn.notMade': 'Qoşulmadı', 'conn.failed': 'Alınmadı: {e}', 'share.stopped': 'Paylaşım dayandı', 'diag.see': ' — Ayarlar → Diaqnostika',
      'conn.noRoute': 'Qoşulmadı (NAT). Ayarlar → TURN.', 'conn.noRouteTurn': 'Qoşulmadı — TURN də işləmədi.', 'conn.acceptTimeout': 'Qəbul vaxtı bitdi',
      'req.sending': 'Sorğu göndərilir…', 'req.sendFailed': 'Göndərilmədi — internet?', 'req.waiting': 'Qəbul gözlənilir…',
      'req.timeout': 'Cavab yoxdur — ünvan səhv və ya oflayn.', 'req.rejected': 'Rədd edildi', 'req.busy': 'Məşğuldur', 'req.accepted': 'Qəbul edildi — qoşulur…',
      'relay.speed': '{who}: TURN — "Sürət" seçildi', 'fs.hint': 'Panel: yuxarı kənar · Ctrl+Alt+Shift+F',
      'id.copied': 'Kopyalandı', 'id.new': 'Yeni ünvan yaradıldı', 'addr.invalid': 'Ünvan 9 rəqəmdir', 'addr.self': 'Bu, öz ünvanınızdır',
      'time.now': 'indicə', 'time.min': '{n} dəq', 'time.hour': '{n} saat',
      'pr.online': 'Onlayn', 'pr.busy': 'Məşğul', 'pr.offline': 'Oflayn', 'pr.checking': '…', 'pr.shown': 'Status görünür', 'pr.hidden': 'Status gizlidir',
      'nat.blocked': 'UDP bloklanıb — yalnız LAN.', 'nat.open': 'Public IP — əla.', 'nat.symmetric': 'Simmetrik NAT — TURN lazım ola bilər.',
      'nat.multi': 'Bir neçə public IP — adətən işləyir.', 'nat.cone': 'Adi NAT — P2P işləyir.',
      'cf.badInput': 'Token ID və ya API Token səhvdir', 'cf.http': 'Cloudflare: HTTP {status}', 'cf.auth': 'Cloudflare: token səhvdir',
      'cf.notFound': 'Cloudflare: Token ID tapılmadı', 'cf.noTurn': 'Cloudflare: TURN yoxdur', 'cf.net': 'Cloudflare-ə çatmaq olmadı',
      'ov.click': 'klikləyir', 'ov.scroll': 'sürüşdürür', 'ov.type': 'yazır',
    },
    en: {
      'home': 'Home', 'desk.title': 'This desk', 'desk.sub': 'Your address',
      'rv.ready': 'Ready · {n}/{total} channels', 'rv.down': 'No channels — internet/firewall', 'rv.connecting': 'Connecting…',
      'nav.remote': 'Remote desk', 'nav.share': 'Screen sharing', 'nav.settings': 'Settings', 'foot.secure': 'End-to-end encrypted · P2P',
      'addr.ph': 'Remote address', 'tt.connect': 'Connect', 'tt.copy': 'Copy', 'tt.settings': 'Settings',
      'perm.banner': 'macOS permissions are needed for remote control.', 'perm.fix': 'Grant',
      'recent.title': 'Recent sessions', 'recent.empty': 'No sessions', 'recent.emptyHint': 'Enter an address above to connect.',
      'perm.title': 'Permissions', 'perm.screen': 'Screen Recording', 'perm.screenHint': 'To share the screen',
      'perm.open': 'Open settings', 'perm.ax': 'Accessibility', 'perm.axHint': 'For mouse and keyboard', 'perm.grant': 'Grant',
      'share.screen': 'Shared screen', 'share.defaults': 'Default permissions', 'perm.control': 'Keyboard & mouse', 'perm.clipboard': 'Clipboard',
      'share.addr': 'Address', 'share.presence': 'Show my online status', 'share.newId': 'New address', 'share.newIdHint': 'The old address will stop working.',
      'share.noScreen': 'No screen found — grant "Screen Recording".',
      'set.lang': 'Language', 'lang.system': 'System language',
      'diag.title': 'Diagnostics', 'diag.hint': 'If connecting fails, check on both sides.', 'diag.run': 'Check network', 'diag.copy': 'Copy log',
      'diag.running': 'Checking…', 'diag.error': 'Error: {e}', 'diag.noLog': 'No attempt yet', 'diag.copied': 'Log copied',
      'turn.title': 'TURN relay', 'turn.hint': 'For mobile internet. Used only when there is no direct path; one side is enough.',
      'turn.enable': 'Use TURN', 'turn.cf': 'Cloudflare · 1000 GB/month free', 'turn.manual': 'Other TURN',
      'turn.cfSteps': '<b>dash.cloudflare.com</b> → Realtime → TURN Server → Create. Paste the Token ID and API Token here.',
      'turn.keyId': 'Token ID', 'turn.token': 'API Token', 'turn.urls': 'Addresses (one per line)', 'turn.user': 'Username', 'turn.pass': 'Password',
      'turn.needCf': 'Enter Token ID and API Token', 'turn.needUrl': 'Enter a turn: address', 'turn.savedErr': 'Saved, but: {e}',
      'turn.saved': 'Saved', 'turn.off': 'TURN disabled', 'turn.works': 'Works — press "Save".', 'turn.none': 'Not working — check the details.',
      'turn.noResp': 'no response', 'turn.notReady': 'TURN not ready: {e}',
      'stun.title': 'STUN servers', 'stun.hint': 'stun: addresses only.', 'stun.saved': 'Saved', 'stun.reset': 'Defaults restored',
      'btn.save': 'Save', 'btn.test': 'Test', 'btn.default': 'Default', 'btn.cancel': 'Cancel', 'btn.close': 'Close',
      'q.auto': 'Auto', 'conn.fallback': 'Trying another route…', 'q.speed': 'Speed', 'q.balanced': 'Balanced', 'q.quality': 'Quality',
      'tt.chat': 'Chat', 'tt.mic': 'Microphone', 'tt.micOn': 'Microphone on', 'tt.file': 'Send file', 'tt.quality': 'Quality / traffic',
      'tt.fs': 'Full screen (Ctrl+Alt+Shift+F)', 'tt.fsExit': 'Normal mode (Ctrl+Alt+Shift+F)', 'tt.remoteMic': 'Remote microphone on',
      'tt.disconnect': 'Disconnect', 'tt.closeTab': 'Close', 'tt.paused': 'In background: mic and sound paused', 'tt.collapse': 'Collapse', 'tt.remove': 'Remove', 'tt.sas': 'Security code',
      'sess.click': 'Click to control', 'sess.viewOnly': 'View only', 'sess.drop': 'Drop file here', 'sas': 'Security code:',
      'perm.active': 'Control', 'perm.view': 'View',
      'chat.title': 'Chat', 'chat.send': 'Send', 'chat.ph': 'Message…', 'chat.empty': 'No messages',
      'acc.wants': 'wants to connect', 'acc.accept': 'Accept', 'acc.reject': 'Decline', 'acc.left': '{s} s', 'acc.live': '{n} connected', 'acc.queue': '{n} waiting',
      'host.connecting': 'connecting…', 'host.connected': 'connected', 'host.remoteMic': 'Microphone on', 'host.disconnect': 'Disconnect', 'host.unknown': 'Unknown',
      'host.joined': '{who} connected', 'notif.request': 'Connection request', 'notif.requestBody': '{who} wants to connect', 'req.cancelled': 'Request cancelled',
      'file.reveal': 'Show', 'file.sending': '{who}: sending {f}', 'file.sent': '{who}: sent {f}', 'file.failed': '{who}: failed {f} — {e}',
      'file.receiving': '{who}: receiving {f}', 'file.received': '{who}: received {f}', 'file.noChannel': 'file channel closed', 'file.closed': 'session closed',
      'st.relay': 'TURN relay', 'st.lan': 'LAN', 'st.total': '{mb} MB total',
      'warn.noStun': 'STUN unreachable — UDP blocked, only LAN will work.',
      'mic.noPerm': 'no microphone permission', 'mic.failed': 'Microphone: {e}', 'mic.noAudio': 'no audio channel',
      'err.noScreen': 'no screen permission', 'err.noAnswer': 'No answer', 'err.relayRejected': 'channels rejected it', 'err.badAnswer': 'Bad answer: {e}',
      'err.answerNotSent': 'answer not sent', 'host.failed': '{who}: failed — {e}',
      'peer.ended': 'Ended by the other side', 'peer.left': 'The other side left', 'sess.ended': 'session ended', 'conn.lost': 'Connection lost',
      'conn.notMade': 'Not connected', 'conn.failed': 'Failed: {e}', 'share.stopped': 'Sharing stopped', 'diag.see': ' — Settings → Diagnostics',
      'conn.noRoute': 'Not connected (NAT). Settings → TURN.', 'conn.noRouteTurn': 'Not connected — TURN failed too.', 'conn.acceptTimeout': 'Accept timed out',
      'req.sending': 'Sending request…', 'req.sendFailed': 'Not sent — internet?', 'req.waiting': 'Waiting for accept…',
      'req.timeout': 'No answer — wrong address or offline.', 'req.rejected': 'Declined', 'req.busy': 'Busy', 'req.accepted': 'Accepted — connecting…',
      'relay.speed': '{who}: TURN — "Speed" selected', 'fs.hint': 'Toolbar: top edge · Ctrl+Alt+Shift+F',
      'id.copied': 'Copied', 'id.new': 'New address created', 'addr.invalid': 'Address has 9 digits', 'addr.self': 'This is your own address',
      'time.now': 'now', 'time.min': '{n} min', 'time.hour': '{n} h',
      'pr.online': 'Online', 'pr.busy': 'Busy', 'pr.offline': 'Offline', 'pr.checking': '…', 'pr.shown': 'Status visible', 'pr.hidden': 'Status hidden',
      'nat.blocked': 'UDP blocked — LAN only.', 'nat.open': 'Public IP — great.', 'nat.symmetric': 'Symmetric NAT — TURN may be needed.',
      'nat.multi': 'Several public IPs — usually works.', 'nat.cone': 'Normal NAT — P2P works.',
      'cf.badInput': 'Invalid Token ID or API Token', 'cf.http': 'Cloudflare: HTTP {status}', 'cf.auth': 'Cloudflare: wrong token',
      'cf.notFound': 'Cloudflare: Token ID not found', 'cf.noTurn': 'Cloudflare: no TURN', 'cf.net': 'Cannot reach Cloudflare',
      'ov.click': 'clicking', 'ov.scroll': 'scrolling', 'ov.type': 'typing',
    },
    ru: {
      'home': 'Главная', 'desk.title': 'Этот стол', 'desk.sub': 'Ваш адрес',
      'rv.ready': 'Готов · {n}/{total} каналов', 'rv.down': 'Нет каналов — интернет/файрвол', 'rv.connecting': 'Подключение…',
      'nav.remote': 'Удалённый стол', 'nav.share': 'Демонстрация экрана', 'nav.settings': 'Настройки', 'foot.secure': 'Сквозное шифрование · P2P',
      'addr.ph': 'Удалённый адрес', 'tt.connect': 'Подключиться', 'tt.copy': 'Копировать', 'tt.settings': 'Настройки',
      'perm.banner': 'Для управления нужны разрешения macOS.', 'perm.fix': 'Разрешить',
      'recent.title': 'Недавние сеансы', 'recent.empty': 'Нет сеансов', 'recent.emptyHint': 'Введите адрес сверху.',
      'perm.title': 'Разрешения', 'perm.screen': 'Запись экрана', 'perm.screenHint': 'Для показа экрана',
      'perm.open': 'Открыть настройки', 'perm.ax': 'Универсальный доступ', 'perm.axHint': 'Для мыши и клавиатуры', 'perm.grant': 'Разрешить',
      'share.screen': 'Общий экран', 'share.defaults': 'Разрешения по умолчанию', 'perm.control': 'Клавиатура и мышь', 'perm.clipboard': 'Буфер',
      'share.addr': 'Адрес', 'share.presence': 'Показывать мой статус', 'share.newId': 'Новый адрес', 'share.newIdHint': 'Старый адрес перестанет работать.',
      'share.noScreen': 'Экран не найден — разрешите «Запись экрана».',
      'set.lang': 'Язык', 'lang.system': 'Язык системы',
      'diag.title': 'Диагностика', 'diag.hint': 'Если не подключается, проверьте на обеих сторонах.', 'diag.run': 'Проверить сеть', 'diag.copy': 'Копировать журнал',
      'diag.running': 'Проверка…', 'diag.error': 'Ошибка: {e}', 'diag.noLog': 'Попыток ещё не было', 'diag.copied': 'Журнал скопирован',
      'turn.title': 'TURN-ретранслятор', 'turn.hint': 'Для мобильного интернета. Только если нет прямого пути; хватит одной стороны.',
      'turn.enable': 'Использовать TURN', 'turn.cf': 'Cloudflare · 1000 ГБ/мес бесплатно', 'turn.manual': 'Другой TURN',
      'turn.cfSteps': '<b>dash.cloudflare.com</b> → Realtime → TURN Server → Create. Вставьте Token ID и API Token.',
      'turn.keyId': 'Token ID', 'turn.token': 'API Token', 'turn.urls': 'Адреса (по одному в строке)', 'turn.user': 'Пользователь', 'turn.pass': 'Пароль',
      'turn.needCf': 'Введите Token ID и API Token', 'turn.needUrl': 'Введите адрес turn:', 'turn.savedErr': 'Сохранено, но: {e}',
      'turn.saved': 'Сохранено', 'turn.off': 'TURN выключен', 'turn.works': 'Работает — нажмите «Сохранить».', 'turn.none': 'Не работает — проверьте данные.',
      'turn.noResp': 'нет ответа', 'turn.notReady': 'TURN не готов: {e}',
      'stun.title': 'STUN-серверы', 'stun.hint': 'Только адреса stun:.', 'stun.saved': 'Сохранено', 'stun.reset': 'Восстановлено',
      'btn.save': 'Сохранить', 'btn.test': 'Проверить', 'btn.default': 'По умолчанию', 'btn.cancel': 'Отмена', 'btn.close': 'Закрыть',
      'q.auto': 'Авто', 'conn.fallback': 'Пробуем другой путь…', 'q.speed': 'Скорость', 'q.balanced': 'Баланс', 'q.quality': 'Качество',
      'tt.chat': 'Чат', 'tt.mic': 'Микрофон', 'tt.micOn': 'Микрофон включён', 'tt.file': 'Отправить файл', 'tt.quality': 'Качество / трафик',
      'tt.fs': 'Полный экран (Ctrl+Alt+Shift+F)', 'tt.fsExit': 'Обычный режим (Ctrl+Alt+Shift+F)', 'tt.remoteMic': 'Микрофон собеседника включён',
      'tt.disconnect': 'Отключиться', 'tt.closeTab': 'Закрыть', 'tt.paused': 'В фоне: микрофон и звук на паузе', 'tt.collapse': 'Свернуть', 'tt.remove': 'Удалить', 'tt.sas': 'Код безопасности',
      'sess.click': 'Нажмите для управления', 'sess.viewOnly': 'Только просмотр', 'sess.drop': 'Отпустите файл', 'sas': 'Код безопасности:',
      'perm.active': 'Управление', 'perm.view': 'Просмотр',
      'chat.title': 'Чат', 'chat.send': 'Отправить', 'chat.ph': 'Сообщение…', 'chat.empty': 'Нет сообщений',
      'acc.wants': 'хочет подключиться', 'acc.accept': 'Принять', 'acc.reject': 'Отклонить', 'acc.left': '{s} с', 'acc.live': 'подключено: {n}', 'acc.queue': 'в очереди: {n}',
      'host.connecting': 'подключается…', 'host.connected': 'подключён', 'host.remoteMic': 'Микрофон включён', 'host.disconnect': 'Отключить', 'host.unknown': 'Неизвестный',
      'host.joined': '{who} подключился', 'notif.request': 'Запрос подключения', 'notif.requestBody': '{who} хочет подключиться', 'req.cancelled': 'Запрос отменён',
      'file.reveal': 'Показать', 'file.sending': '{who}: отправка {f}', 'file.sent': '{who}: отправлено {f}', 'file.failed': '{who}: ошибка {f} — {e}',
      'file.receiving': '{who}: приём {f}', 'file.received': '{who}: получено {f}', 'file.noChannel': 'файловый канал закрыт', 'file.closed': 'сеанс закрыт',
      'st.relay': 'TURN-ретранслятор', 'st.lan': 'LAN', 'st.total': 'всего {mb} МБ',
      'warn.noStun': 'STUN недоступен — UDP заблокирован, работает только LAN.',
      'mic.noPerm': 'нет разрешения на микрофон', 'mic.failed': 'Микрофон: {e}', 'mic.noAudio': 'нет аудиоканала',
      'err.noScreen': 'нет разрешения на экран', 'err.noAnswer': 'Нет ответа', 'err.relayRejected': 'каналы не приняли', 'err.badAnswer': 'Неверный ответ: {e}',
      'err.answerNotSent': 'ответ не отправлен', 'host.failed': '{who}: ошибка — {e}',
      'peer.ended': 'Завершено собеседником', 'peer.left': 'Собеседник вышел', 'sess.ended': 'сеанс завершён', 'conn.lost': 'Связь потеряна',
      'conn.notMade': 'Не подключено', 'conn.failed': 'Ошибка: {e}', 'share.stopped': 'Показ остановлен', 'diag.see': ' — Настройки → Диагностика',
      'conn.noRoute': 'Не подключено (NAT). Настройки → TURN.', 'conn.noRouteTurn': 'Не подключено — TURN тоже не помог.', 'conn.acceptTimeout': 'Время принятия истекло',
      'req.sending': 'Отправка запроса…', 'req.sendFailed': 'Не отправлено — интернет?', 'req.waiting': 'Ожидание принятия…',
      'req.timeout': 'Нет ответа — неверный адрес или офлайн.', 'req.rejected': 'Отклонено', 'req.busy': 'Занят', 'req.accepted': 'Принято — подключение…',
      'relay.speed': '{who}: TURN — выбран режим «Скорость»', 'fs.hint': 'Панель: верхний край · Ctrl+Alt+Shift+F',
      'id.copied': 'Скопировано', 'id.new': 'Создан новый адрес', 'addr.invalid': 'Адрес — 9 цифр', 'addr.self': 'Это ваш собственный адрес',
      'time.now': 'сейчас', 'time.min': '{n} мин', 'time.hour': '{n} ч',
      'pr.online': 'В сети', 'pr.busy': 'Занят', 'pr.offline': 'Не в сети', 'pr.checking': '…', 'pr.shown': 'Статус виден', 'pr.hidden': 'Статус скрыт',
      'nat.blocked': 'UDP заблокирован — только LAN.', 'nat.open': 'Публичный IP — отлично.', 'nat.symmetric': 'Симметричный NAT — может понадобиться TURN.',
      'nat.multi': 'Несколько публичных IP — обычно работает.', 'nat.cone': 'Обычный NAT — P2P работает.',
      'cf.badInput': 'Неверный Token ID или API Token', 'cf.http': 'Cloudflare: HTTP {status}', 'cf.auth': 'Cloudflare: неверный токен',
      'cf.notFound': 'Cloudflare: Token ID не найден', 'cf.noTurn': 'Cloudflare: нет TURN', 'cf.net': 'Нет связи с Cloudflare',
      'ov.click': 'кликает', 'ov.scroll': 'прокручивает', 'ov.type': 'печатает',
    },
    tr: {
      'home': 'Ana sayfa', 'desk.title': 'Bu masa', 'desk.sub': 'Adresiniz',
      'rv.ready': 'Hazır · {n}/{total} kanal', 'rv.down': 'Kanal yok — internet/güvenlik duvarı', 'rv.connecting': 'Bağlanıyor…',
      'nav.remote': 'Uzak masa', 'nav.share': 'Ekran paylaşımı', 'nav.settings': 'Ayarlar', 'foot.secure': 'Uçtan uca şifreli · P2P',
      'addr.ph': 'Uzak adres', 'tt.connect': 'Bağlan', 'tt.copy': 'Kopyala', 'tt.settings': 'Ayarlar',
      'perm.banner': 'Uzaktan kontrol için macOS izinleri gerekli.', 'perm.fix': 'İzin ver',
      'recent.title': 'Son oturumlar', 'recent.empty': 'Oturum yok', 'recent.emptyHint': 'Yukarıya adres yazıp bağlanın.',
      'perm.title': 'İzinler', 'perm.screen': 'Ekran Kaydı', 'perm.screenHint': 'Ekranı paylaşmak için',
      'perm.open': 'Ayarları aç', 'perm.ax': 'Erişilebilirlik', 'perm.axHint': 'Fare ve klavye için', 'perm.grant': 'İzin ver',
      'share.screen': 'Paylaşılan ekran', 'share.defaults': 'Varsayılan izinler', 'perm.control': 'Klavye ve fare', 'perm.clipboard': 'Pano',
      'share.addr': 'Adres', 'share.presence': 'Çevrimiçi durumumu göster', 'share.newId': 'Yeni adres', 'share.newIdHint': 'Eski adres çalışmayacak.',
      'share.noScreen': 'Ekran bulunamadı — "Ekran Kaydı" izni verin.',
      'set.lang': 'Dil', 'lang.system': 'Sistem dili',
      'diag.title': 'Tanılama', 'diag.hint': 'Bağlanmıyorsa iki tarafta da kontrol edin.', 'diag.run': 'Ağı kontrol et', 'diag.copy': 'Günlüğü kopyala',
      'diag.running': 'Kontrol ediliyor…', 'diag.error': 'Hata: {e}', 'diag.noLog': 'Henüz deneme yok', 'diag.copied': 'Günlük kopyalandı',
      'turn.title': 'TURN relay', 'turn.hint': 'Mobil internet için. Yalnız doğrudan yol yoksa; bir taraf yeterli.',
      'turn.enable': 'TURN kullan', 'turn.cf': 'Cloudflare · ayda 1000 GB ücretsiz', 'turn.manual': 'Diğer TURN',
      'turn.cfSteps': '<b>dash.cloudflare.com</b> → Realtime → TURN Server → Create. Token ID ve API Token’ı buraya yapıştırın.',
      'turn.keyId': 'Token ID', 'turn.token': 'API Token', 'turn.urls': 'Adresler (satır başına bir)', 'turn.user': 'Kullanıcı', 'turn.pass': 'Parola',
      'turn.needCf': 'Token ID ve API Token girin', 'turn.needUrl': 'Bir turn: adresi girin', 'turn.savedErr': 'Kaydedildi, ama: {e}',
      'turn.saved': 'Kaydedildi', 'turn.off': 'TURN kapatıldı', 'turn.works': 'Çalışıyor — "Kaydet"e basın.', 'turn.none': 'Çalışmıyor — bilgileri kontrol edin.',
      'turn.noResp': 'yanıt yok', 'turn.notReady': 'TURN hazır değil: {e}',
      'stun.title': 'STUN sunucuları', 'stun.hint': 'Yalnız stun: adresleri.', 'stun.saved': 'Kaydedildi', 'stun.reset': 'Varsayılanlar yüklendi',
      'btn.save': 'Kaydet', 'btn.test': 'Test et', 'btn.default': 'Varsayılan', 'btn.cancel': 'İptal', 'btn.close': 'Kapat',
      'q.auto': 'Otomatik', 'conn.fallback': 'Başka yol deneniyor…', 'q.speed': 'Hız', 'q.balanced': 'Dengeli', 'q.quality': 'Kalite',
      'tt.chat': 'Sohbet', 'tt.mic': 'Mikrofon', 'tt.micOn': 'Mikrofon açık', 'tt.file': 'Dosya gönder', 'tt.quality': 'Kalite / trafik',
      'tt.fs': 'Tam ekran (Ctrl+Alt+Shift+F)', 'tt.fsExit': 'Normal mod (Ctrl+Alt+Shift+F)', 'tt.remoteMic': 'Karşı tarafın mikrofonu açık',
      'tt.disconnect': 'Ayrıl', 'tt.closeTab': 'Kapat', 'tt.paused': 'Arka planda: mikrofon ve ses durdu', 'tt.collapse': 'Küçült', 'tt.remove': 'Sil', 'tt.sas': 'Güvenlik kodu',
      'sess.click': 'Kontrol için tıklayın', 'sess.viewOnly': 'Yalnız izleme', 'sess.drop': 'Dosyayı bırakın', 'sas': 'Güvenlik kodu:',
      'perm.active': 'Kontrol', 'perm.view': 'İzleme',
      'chat.title': 'Sohbet', 'chat.send': 'Gönder', 'chat.ph': 'Mesaj…', 'chat.empty': 'Mesaj yok',
      'acc.wants': 'bağlanmak istiyor', 'acc.accept': 'Kabul et', 'acc.reject': 'Reddet', 'acc.left': '{s} sn', 'acc.live': '{n} bağlı', 'acc.queue': '{n} sırada',
      'host.connecting': 'bağlanıyor…', 'host.connected': 'bağlı', 'host.remoteMic': 'Mikrofonu açık', 'host.disconnect': 'Ayır', 'host.unknown': 'Bilinmiyor',
      'host.joined': '{who} bağlandı', 'notif.request': 'Bağlantı isteği', 'notif.requestBody': '{who} bağlanmak istiyor', 'req.cancelled': 'İstek iptal edildi',
      'file.reveal': 'Göster', 'file.sending': '{who}: gönderiliyor {f}', 'file.sent': '{who}: gönderildi {f}', 'file.failed': '{who}: başarısız {f} — {e}',
      'file.receiving': '{who}: alınıyor {f}', 'file.received': '{who}: alındı {f}', 'file.noChannel': 'dosya kanalı kapalı', 'file.closed': 'oturum kapandı',
      'st.relay': 'TURN relay', 'st.lan': 'LAN', 'st.total': 'toplam {mb} MB',
      'warn.noStun': 'STUN erişilemiyor — UDP engelli, yalnız LAN çalışır.',
      'mic.noPerm': 'mikrofon izni yok', 'mic.failed': 'Mikrofon: {e}', 'mic.noAudio': 'ses kanalı yok',
      'err.noScreen': 'ekran izni yok', 'err.noAnswer': 'Yanıt yok', 'err.relayRejected': 'kanallar kabul etmedi', 'err.badAnswer': 'Hatalı yanıt: {e}',
      'err.answerNotSent': 'yanıt gönderilmedi', 'host.failed': '{who}: başarısız — {e}',
      'peer.ended': 'Karşı taraf bitirdi', 'peer.left': 'Karşı taraf ayrıldı', 'sess.ended': 'oturum bitti', 'conn.lost': 'Bağlantı koptu',
      'conn.notMade': 'Bağlanmadı', 'conn.failed': 'Başarısız: {e}', 'share.stopped': 'Paylaşım durdu', 'diag.see': ' — Ayarlar → Tanılama',
      'conn.noRoute': 'Bağlanmadı (NAT). Ayarlar → TURN.', 'conn.noRouteTurn': 'Bağlanmadı — TURN da çalışmadı.', 'conn.acceptTimeout': 'Kabul süresi doldu',
      'req.sending': 'İstek gönderiliyor…', 'req.sendFailed': 'Gönderilemedi — internet?', 'req.waiting': 'Kabul bekleniyor…',
      'req.timeout': 'Yanıt yok — adres yanlış veya çevrimdışı.', 'req.rejected': 'Reddedildi', 'req.busy': 'Meşgul', 'req.accepted': 'Kabul edildi — bağlanıyor…',
      'relay.speed': '{who}: TURN — "Hız" seçildi', 'fs.hint': 'Araç çubuğu: üst kenar · Ctrl+Alt+Shift+F',
      'id.copied': 'Kopyalandı', 'id.new': 'Yeni adres oluşturuldu', 'addr.invalid': 'Adres 9 hanelidir', 'addr.self': 'Bu sizin adresiniz',
      'time.now': 'şimdi', 'time.min': '{n} dk', 'time.hour': '{n} sa',
      'pr.online': 'Çevrimiçi', 'pr.busy': 'Meşgul', 'pr.offline': 'Çevrimdışı', 'pr.checking': '…', 'pr.shown': 'Durum görünür', 'pr.hidden': 'Durum gizli',
      'nat.blocked': 'UDP engelli — yalnız LAN.', 'nat.open': 'Genel IP — harika.', 'nat.symmetric': 'Simetrik NAT — TURN gerekebilir.',
      'nat.multi': 'Birden çok genel IP — genelde çalışır.', 'nat.cone': 'Normal NAT — P2P çalışır.',
      'cf.badInput': 'Token ID veya API Token hatalı', 'cf.http': 'Cloudflare: HTTP {status}', 'cf.auth': 'Cloudflare: token hatalı',
      'cf.notFound': 'Cloudflare: Token ID bulunamadı', 'cf.noTurn': 'Cloudflare: TURN yok', 'cf.net': 'Cloudflare’a ulaşılamadı',
      'ov.click': 'tıklıyor', 'ov.scroll': 'kaydırıyor', 'ov.type': 'yazıyor',
    },
  };

  let current = 'en';
  let systemLangs = [];

  function pick(codes) {
    for (const c of codes || []) {
      const base = String(c).toLowerCase().split(/[-_]/)[0];
      if (D[base]) return base;
    }
    return 'en';
  }

  function setting() {
    try { return localStorage.getItem('lang') || 'system'; } catch { return 'system'; }
  }

  function t(key, vars) {
    let s = (D[current] && D[current][key]) ?? D.en[key] ?? key;
    if (vars) s = s.replace(/\{(\w+)\}/g, (m, k) => (vars[k] !== undefined ? String(vars[k]) : m));
    return s;
  }

  // Applies translations to static markup (also inside <template> contents).
  function apply(root = document) {
    const each = (sel, fn) => root.querySelectorAll(sel).forEach(fn);
    each('[data-i18n]', (el) => { el.textContent = t(el.dataset.i18n); });
    each('[data-i18n-html]', (el) => { el.innerHTML = t(el.dataset.i18nHtml); });
    each('[data-i18n-title]', (el) => { el.title = t(el.dataset.i18nTitle); });
    each('[data-i18n-placeholder]', (el) => { el.placeholder = t(el.dataset.i18nPlaceholder); });
    each('[data-i18n-aria]', (el) => { el.setAttribute('aria-label', t(el.dataset.i18nAria)); });
    if (root === document) root.querySelectorAll('template').forEach((tpl) => apply(tpl.content));
  }

  function setLanguage(choice) {
    try { localStorage.setItem('lang', choice); } catch {}
    current = choice === 'system' ? pick(systemLangs) : (D[choice] ? choice : 'en');
    document.documentElement.lang = current;
    apply();
    window.dispatchEvent(new CustomEvent('languagechange-app', { detail: current }));
    return current;
  }

  // System language: Electron reports the OS preference list on macOS, Windows and Linux;
  // navigator.languages is the fallback.
  function init(langs) {
    systemLangs = (langs && langs.length ? langs : navigator.languages) || [];
    return setLanguage(setting());
  }

  window.I18N = { t, apply, init, setLanguage, setting, LANGS, get lang() { return current; }, get system() { return pick(systemLangs); } };
  window.t = t;
  // Usable before init (English until the system language is known).
  current = pick(navigator.languages);
})();
