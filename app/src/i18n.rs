//! UI translations. English strings are the keys; `t()` returns the Turkish text when the
//! interface language is Turkish, and the English key otherwise (also for anything missing).
//! `tf()` fills `{}` placeholders in order.

use std::sync::atomic::{AtomicU8, Ordering};

static LANG: AtomicU8 = AtomicU8::new(0); // 0 = English, 1 = Turkish

pub fn set_lang(code: &str) {
    LANG.store(if code == "tr" { 1 } else { 0 }, Ordering::Relaxed);
}

fn turkish() -> bool {
    LANG.load(Ordering::Relaxed) == 1
}

pub fn t(en: &'static str) -> &'static str {
    if turkish() {
        tr(en).unwrap_or(en)
    } else {
        en
    }
}

pub fn tf(en: &'static str, args: &[&str]) -> String {
    let mut out = t(en).to_string();
    for a in args {
        if let Some(i) = out.find("{}") {
            out.replace_range(i..i + 2, a);
        }
    }
    out
}

/// Progress messages that come from the Python engine helper.
pub fn engine_status(msg: &str) -> String {
    if !turkish() {
        return msg.to_string();
    }
    if let Some(rest) = msg.strip_prefix("Speaking test sentence ") {
        return format!("Test cümlesi okunuyor {rest}");
    }
    match msg {
        "Loading the model…" => "Model yükleniyor…",
        "Warming up…" => "Isınıyor…",
        "Checking voice match…" => "Ses benzerliği ölçülüyor…",
        "Checking clarity with Whisper…" => "Netlik Whisper ile ölçülüyor…",
        "Starting…" => "Başlatılıyor…",
        other => return other.to_string(),
    }
    .to_string()
}

/// Steps reported by the virtual mic setup.
pub fn vmic_step(step: &str) -> String {
    if !turkish() {
        return step.to_string();
    }
    if let Some(rest) = step.strip_prefix("Downloading VB-Cable from vb-audio.com") {
        return format!("VB-Cable vb-audio.com'dan indiriliyor{rest}");
    }
    if let Some(rest) = step.strip_prefix("Download failed: ") {
        return format!("İndirme başarısız: {rest}");
    }
    if let Some(rest) = step.strip_prefix("The installer's signature didn't check out") {
        return format!("Kurulum dosyasının imzası doğrulanamadı{rest}");
    }
    if let Some(rest) = step.strip_prefix("Couldn't start PowerShell: ") {
        return format!("PowerShell başlatılamadı: {rest}");
    }
    match step {
        "Starting…" => "Başlatılıyor…",
        "Approve the Windows prompt, then click Install Driver" => "Windows iznini onayla, sonra Install Driver'a tıkla",
        "Creating the Corvox virtual mic" => "Corvox sanal mikrofonu oluşturuluyor",
        "pactl failed. Is PipeWire or PulseAudio running?" => "pactl başarısız oldu. PipeWire ya da PulseAudio çalışıyor mu?",
        other => return other.to_string(),
    }
    .to_string()
}

/// Language names for the speech-language picker.
pub fn lang_name(code: &str, english: &str) -> String {
    if !turkish() {
        return english.to_string();
    }
    match code {
        "tr" => "Türkçe",
        "en" => "İngilizce",
        "fr" => "Fransızca",
        "de" => "Almanca",
        "es" => "İspanyolca",
        "it" => "İtalyanca",
        "pt" => "Portekizce",
        "nl" => "Felemenkçe",
        "pl" => "Lehçe",
        "ru" => "Rusça",
        "cs" => "Çekçe",
        "ar" => "Arapça",
        "zh" => "Çince",
        "hu" => "Macarca",
        "ko" => "Korece",
        "ja" => "Japonca",
        "hi" => "Hintçe",
        "da" => "Danca",
        "el" => "Yunanca",
        "fi" => "Fince",
        "he" => "İbranice",
        "ms" => "Malayca",
        "no" => "Norveççe",
        "sv" => "İsveççe",
        "sw" => "Svahili",
        _ => english,
    }
    .to_string()
}

fn tr(en: &str) -> Option<&'static str> {
    Some(match en {
        // status
        "Ready" => "Hazır",
        "Starting engine…" => "Motor başlatılıyor…",
        "Loading the model…" => "Model yükleniyor…",
        "Generating…" => "Üretiliyor…",
        "Speaking" => "Konuşuyor",
        "Speaking (saved audio)" => "Konuşuyor (kayıtlı ses)",
        "Model downloaded" => "Model indirildi",
        "Engine installed" => "Motor kuruldu",
        "Downloading model {}" => "Model indiriliyor {}",
        "Downloading {}" => "İndiriliyor {}",

        // errors and messages
        "Couldn't register {}: {}" => "{} kaydedilemedi: {}",
        "Bad shortcut \"{}\": {}" => "Geçersiz kısayol \"{}\": {}",
        "Couldn't start engine: {}" => "Motor başlatılamadı: {}",
        "Add your voice clip first." => "Önce ses kaydını ekle.",
        "Install {} first." => "Önce {} kur.",
        "{} is still downloading. Pick another engine meanwhile." => "{} hâlâ iniyor. Bu arada başka bir motor seçebilirsin.",
        "Engine error: {}" => "Motor hatası: {}",
        "Engine stopped. {}" => "Motor durdu. {}",
        "Download stopped. {}" => "İndirme durdu. {}",
        "Added \"{}\" ({} s)" => "\"{}\" eklendi ({} sn)",
        "Couldn't use that file: {}" => "Bu dosya kullanılamadı: {}",
        "Install failed: {}" => "Kurulum başarısız: {}",
        "Test failed: {}" => "Test başarısız: {}",
        "Virtual mic ready. In Discord: Settings > Voice & Video > Input Device > {}." => {
            "Sanal mikrofon hazır. Discord'da: Ayarlar > Ses ve Görüntü > Giriş Cihazı > {}."
        }
        "Installed. Restart your PC to finish, then open Corvox again." => "Kuruldu. Bitirmek için bilgisayarı yeniden başlat, sonra Corvox'u tekrar aç.",
        "Cancelled: the Windows permission prompt was declined." => "İptal edildi: Windows izni reddedildi.",
        "Stopped: the installer isn't signed by VB-Audio. Nothing was installed." => {
            "Durduruldu: kurulum dosyası VB-Audio imzalı değil. Hiçbir şey kurulmadı."
        }
        "Couldn't download VB-Cable. Check your internet connection." => "VB-Cable indirilemedi. İnternet bağlantını kontrol et.",
        "Setup failed (code {})." => "Kurulum başarısız (kod {}).",

        // window and main page
        "Close" => "Kapat",
        "On top of other windows (click to turn off)" => "Diğer pencerelerin üstünde (kapatmak için tıkla)",
        "Keep on top of other windows" => "Diğer pencerelerin üstünde tut",
        "Collapse to a bar" => "Çubuğa küçült",
        "Settings" => "Ayarlar",
        "Type and press Enter…" => "Yaz ve Enter'a bas…",
        "Stop" => "Durdur",
        "Stop speaking (Esc)" => "Konuşmayı durdur (Esc)",
        "Stop (Esc)" => "Durdur (Esc)",
        "Speak (Enter)" => "Söyle (Enter)",
        "Expand" => "Büyüt",
        "Dismiss" => "Kapat",
        "Volume on your speakers/headphones. Discord always gets full volume." => "Hoparlör/kulaklık ses seviyesi. Discord her zaman tam sesi alır.",
        "Hearing yourself: on (click to mute)" => "Kendini duyma: açık (kapatmak için tıkla)",
        "Hearing yourself: off (click to unmute)" => "Kendini duyma: kapalı (açmak için tıkla)",
        "Recent" => "Son kullanılanlar",
        "Pinned" => "Sabitlenenler",
        "↑↓ to choose · Enter speaks · right-click to edit" => "↑↓ ile seç · Enter söyler · sağ tık düzenler",
        "Phrases you speak show up here. Click one to say it again." => "Söylediğin cümleler burada görünür. Tekrar söylemek için birine tıkla.",
        "Phrase history is off. Pin a phrase to keep it here." => "Cümle geçmişi kapalı. Burada tutmak için bir cümleyi sabitle.",
        "Speak (saved audio, plays instantly)" => "Söyle (kayıtlı ses, anında çalar)",
        "Speak" => "Söyle",
        "Click to speak, right-click to edit" => "Söylemek için tıkla, düzenlemek için sağ tıkla",
        "Unpin" => "Sabitlemeyi kaldır",
        "Pin" => "Sabitle",
        "Remove" => "Kaldır",
        "Add voice" => "Ses ekle",
        "Voices" => "Sesler",
        "Engine" => "Motor",
        "Engines and their stats" => "Motorlar ve ölçümleri",
        "Speech language" => "Konuşma dili",

        // virtual mic
        "Install BlackHole 2ch from that page, then click Refresh here." => "O sayfadan BlackHole 2ch'yi kur, sonra burada Yenile'ye tıkla.",
        "Starting…" => "Başlatılıyor…",
        "No virtual mic yet, so Discord can't hear Corvox." => "Henüz sanal mikrofon yok, bu yüzden Discord Corvox'u duyamaz.",
        "Set up" => "Kur",
        "Discord needs a microphone to hear Corvox. VB-Cable is a free virtual audio cable from VB-Audio: Corvox plays into it and Discord listens to it. Windows will ask for permission, then click Install Driver in VB-Audio's installer." => {
            "Discord'un Corvox'u duyması için bir mikrofon gerekir. VB-Cable, VB-Audio'nun ücretsiz sanal ses kablosudur: Corvox içine çalar, Discord onu dinler. Windows izin isteyecek, sonra VB-Audio kurulumunda Install Driver'a tıkla."
        }
        "Discord needs a microphone to hear Corvox. BlackHole is a free virtual audio cable: install BlackHole 2ch, and Corvox plays into it while Discord listens to it." => {
            "Discord'un Corvox'u duyması için bir mikrofon gerekir. BlackHole ücretsiz bir sanal ses kablosudur: BlackHole 2ch'yi kur; Corvox içine çalar, Discord onu dinler."
        }
        "Discord needs a microphone to hear Corvox. Corvox can create a virtual mic with PipeWire/PulseAudio, no install needed." => {
            "Discord'un Corvox'u duyması için bir mikrofon gerekir. Corvox, PipeWire/PulseAudio ile kurulum gerektirmeden sanal mikrofon oluşturabilir."
        }
        "Get BlackHole" => "BlackHole'u indir",
        "Set up virtual mic" => "Sanal mikrofonu kur",

        // engines
        "Back" => "Geri",
        "Engines" => "Motorlar",
        "Couldn't find engines/catalog.json next to the app." => "Uygulamanın yanında engines/catalog.json bulunamadı.",
        "Engines · {}" => "Motorlar · {}",
        "All languages" => "Tüm diller",
        "uv isn't installed, so engines can't be installed. See the README." => "uv kurulu değil, bu yüzden motorlar kurulamaz. README'ye bak.",
        "Voice match compares the output to your clip. Clarity is how well Whisper understands it. Speed is seconds of speech per second of work." => {
            "Ses benzerliği çıktıyı kaydınla karşılaştırır. Netlik, Whisper'ın onu ne kadar iyi anladığıdır. Hız, bir saniyelik işte üretilen konuşma süresidir."
        }
        "Voice match" => "Ses benzerliği",
        "Clarity" => "Netlik",
        "Speed" => "Hız",
        "{}× real time" => "gerçek zamanın {}×'i",
        "VRAM (ran on CPU)" => "VRAM (işlemcide çalıştı)",
        "Measured on your PC · {} · voice \"{}\"{}" => "Senin bilgisayarında ölçüldü · {} · ses \"{}\"{}",
        "Published · {}" => "Yayınlanan · {}",
        "no numbers yet" => "henüz ölçüm yok",
        "CPU" => "İşlemci",
        "Needs a free Hugging Face login to clone voices." => "Ses klonlamak için ücretsiz Hugging Face girişi gerekir.",
        "No CUDA GPU found, so this engine is running on the CPU." => "CUDA ekran kartı bulunamadı, bu yüzden motor işlemcide çalışıyor.",
        "Step 1 of 2 · Installing Python packages" => "Adım 1/2 · Python paketleri kuruluyor",
        "{} of ~{}" => "{} / ~{}",
        "Pause" => "Duraklat",
        "Install paused · {} of ~{}" => "Kurulum duraklatıldı · {} / ~{}",
        "Resume" => "Devam et",
        "Step 2 of 2 · Downloading the model (switches when done)" => "Adım 2/2 · Model indiriliyor (bitince geçilecek)",
        "Step 2 of 2 · Downloading the model" => "Adım 2/2 · Model indiriliyor",
        "Your current engine keeps working meanwhile." => "Bu sırada şu anki motorun çalışmaya devam eder.",
        "Model download paused · {} of ~{}" => "Model indirmesi duraklatıldı · {} / ~{}",
        "Model download paused" => "Model indirmesi duraklatıldı",
        "Installed · model not downloaded yet (~{})" => "Kurulu · model henüz indirilmedi (~{})",
        "Download" => "İndir",
        "I agree, install" => "Kabul ediyorum, kur",
        "Cancel" => "İptal",
        "Install" => "Kur",
        "Test on my PC" => "Bilgisayarımda test et",
        "Add a voice clip first" => "Önce bir ses kaydı ekle",
        "info" => "bilgi",
        "installed" => "kurulu",
        "~{} model" => "~{} model",
        "~{} on disk" => "diskte ~{}",

        // voices
        "Use 10–20 seconds of you talking normally: no music, no background noise. Corvox uses the part that ends at a natural pause." => {
            "Normal konuştuğun 10–20 saniyelik bir kayıt kullan: müzik ve arka plan gürültüsü olmasın. Corvox kaydın doğal bir duraklamayla biten kısmını kullanır."
        }
        "{} s clip" => "{} sn kayıt",
        "Delete" => "Sil",
        "Play the clip" => "Kaydı çal",
        "Preparing the clip…" => "Kayıt hazırlanıyor…",
        "Add voice from a file…" => "Dosyadan ses ekle…",
        "Pick a clip of your voice" => "Sesinin bir kaydını seç",
        "Audio" => "Ses",

        // settings
        "Interface language" => "Arayüz dili",
        "Send the voice to" => "Sesi şuraya gönder",
        "Auto ({})" => "Otomatik ({})",
        "no virtual cable found" => "sanal kablo bulunamadı",
        "Auto" => "Otomatik",
        "Refresh devices" => "Cihazları yenile",
        "Phrase history" => "Cümle geçmişi",
        "Remember recent phrases and their audio" => "Son cümleleri ve seslerini hatırla",
        "Keep the last" => "Son",
        "phrases" => "cümleyi tut",
        "Saved audio replays instantly without the engine. Older phrases and their audio are deleted; pinned ones stay. Turning this off clears the list and the saved audio." => {
            "Kayıtlı ses motor olmadan anında çalar. Daha eski cümleler ve sesleri silinir; sabitlenenler kalır. Bunu kapatmak listeyi ve kayıtlı sesleri temizler."
        }
        "Faster, lighter Pocket engines (int8)" => "Daha hızlı, daha hafif Pocket motorları (int8)",
        "About 10% faster and ~450 MB less RAM. Re-run Test on my PC to compare." => {
            "Yaklaşık %10 daha hızlı ve ~450 MB daha az RAM. Karşılaştırmak için testi tekrar çalıştır."
        }
        "Push-to-talk key (held while speaking)" => "Bas-konuş tuşu (konuşurken basılı tutulur)",
        "Off" => "Kapalı",
        "Set the same key as Push to Talk in Discord. F13–F24 don't clash with anything." => {
            "Discord'da Bas-Konuş için aynı tuşu ayarla. F13–F24 hiçbir şeyle çakışmaz."
        }
        "Shortcut: bring Corvox up, press again to go back" => "Kısayol: Corvox'u öne getirir, tekrar basınca geri döner",
        "Apply" => "Uygula",
        "Hugging Face login (for Kyutai's Pocket TTS voices)" => "Hugging Face girişi (Kyutai'nin Pocket TTS sesleri için)",
        "Token saved" => "Anahtar kaydedildi",
        "Accept the terms on kyutai/pocket-tts" => "kyutai/pocket-tts'te koşulları kabul et",
        "Create a read token" => "Okuma anahtarı oluştur",
        "Save token" => "Anahtarı kaydet",
        "Saved. Restarting the engine." => "Kaydedildi. Motor yeniden başlatılıyor.",
        "Couldn't save: {}" => "Kaydedilemedi: {}",
        "Paths" => "Klasörler",
        "not found" => "bulunamadı",
        "not installed" => "kurulu değil",
        "Engines: {}" => "Motorlar: {}",
        "Data: {}" => "Veriler: {}",

        // downloads, deleting, cache
        "{} is ready" => "{} hazır",
        "Downloading {} {}" => "{} indiriliyor {}",
        "Installing {}…" => "{} kuruluyor…",
        "Deleted {} (freed {})" => "{} silindi ({} boşaldı)",
        "Couldn't delete everything: {}" => "Her şey silinemedi: {}",
        "Deleting…" => "Siliniyor…",
        "Delete {}'s packages and model (about {})? You can install it again later." => {
            "{} paketleri ve modeli silinsin mi (yaklaşık {})? Daha sonra tekrar kurabilirsin."
        }
        "This also removes {}, which share the same install." => "Aynı kurulumu paylaştıkları için {} de kaldırılır.",
        "Delete this engine's files" => "Bu motorun dosyalarını sil",
        "Download cache" => "İndirme önbelleği",
        "uv keeps downloaded packages so reinstalling is fast. Clearing it frees space; installed engines keep working." => {
            "uv indirilen paketleri saklar, böylece yeniden kurmak hızlı olur. Temizlemek yer açar; kurulu motorlar çalışmaya devam eder."
        }
        "Clearing…" => "Temizleniyor…",
        "Clear download cache" => "İndirme önbelleğini temizle",
        "Wait for installs to finish" => "Kurulumların bitmesini bekle",
        "Download cache cleared. {}" => "İndirme önbelleği temizlendi. {}",
        "Couldn't clear the cache: {}" => "Önbellek temizlenemedi: {}",
        _ => return None,
    })
}
