//! Deterministic safety layer. Runs BEFORE the LLM so emergencies never depend on model behaviour.

use serde::Serialize;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Emergency {
    Medical,
    SelfHarm,
}

const SELF_HARM: &[&str] = &[
    "suicide", "kill myself", "end my life", "want to die", "self harm", "self-harm", "hurt myself",
    "आत्महत्या", "मरना चाहता", "मरना चाहती", "जान दे", "खुद को खत्म",
    "atmahatya", "marna chahta", "marna chahti", "khud ko khatam",
];

const MEDICAL: &[&str] = &[
    "chest pain", "chest tightness", "crushing chest", "heart attack", "can't breathe", "cannot breathe",
    "cant breathe", "can not breathe", "unable to breathe", "not able to breathe", "stopped breathing", "can't get air", "struggling to breathe", "difficulty breathing", "gasping", "unconscious",
    "passed out", "not responding", "unresponsive", "seizure", "convulsion", "stroke", "face drooping",
    "slurred speech", "severe bleeding", "bleeding heavily", "won't stop bleeding", "overdose",
    "swallowed poison", "poisoning", "choking", "throat swelling", "anaphylaxis", "vomiting blood",
    "coughing blood",
    "सीने में दर्द", "सीने में जकड़न", "सांस नहीं", "साँस नहीं", "सांस लेने में तकलीफ", "बेहोश",
    "दौरा", "लकवा", "खून बह", "जहर", "दिल का दौरा", "खून की उल्टी",
    "seene mein dard", "sine mein dard", "sans nahi", "saans nahi", "saans lene mein takleef",
    "behosh", "dil ka daura", "lakwa", "zeher",
];

pub fn detect_emergency(text: &str) -> Option<Emergency> {
    let t = text.to_lowercase().replace('’', "'");
    // Self-harm first: it needs a different, supportive response.
    if SELF_HARM.iter().any(|k| t.contains(k)) {
        return Some(Emergency::SelfHarm);
    }
    if MEDICAL.iter().any(|k| t.contains(k)) {
        return Some(Emergency::Medical);
    }
    None
}

pub fn has_devanagari(text: &str) -> bool {
    text.chars().any(|c| ('\u{0900}'..='\u{097F}').contains(&c))
}

/// Fixed, vetted emergency reply. India defaults: 112 (all emergencies), 108 (ambulance), Tele-MANAS 14416.
pub fn emergency_message(kind: Emergency, hindi: bool) -> &'static str {
    match (kind, hindi) {
        (Emergency::Medical, false) => "This could be a medical emergency. Please call 112 (or 108 for an ambulance in India) or go to the nearest emergency room right now. If you feel faint, don't go alone or drive yourself; ask someone nearby to help. If this isn't happening right now, tell me more and I'll help.",
        (Emergency::Medical, true) => "यह एक मेडिकल इमरजेंसी हो सकती है। कृपया अभी 112 (या एम्बुलेंस के लिए 108) पर कॉल करें या नज़दीकी अस्पताल के इमरजेंसी विभाग में जाएँ। चक्कर जैसा लगे तो अकेले या खुद गाड़ी चलाकर न जाएँ, किसी पास के व्यक्ति से मदद लें। अगर यह अभी नहीं हो रहा, तो मुझे विस्तार से बताइए, मैं मदद करूँगा।",
        (Emergency::SelfHarm, false) => "I'm really sorry you're going through this, and I'm glad you told me. You deserve support right now. In India you can call Tele-MANAS at 14416 (free, 24/7), or 112 in an emergency. If you're in immediate danger, call 112 or go to the nearest hospital, and please be with someone you trust. I'm here to keep talking with you too.",
        (Emergency::SelfHarm, true) => "आप जिस दौर से गुज़र रहे हैं, उसके लिए मुझे सच में दुख है, और मुझे खुशी है कि आपने मुझे बताया। आपको अभी सहारा मिलना चाहिए। भारत में आप Tele-MANAS को 14416 पर (मुफ़्त, 24x7) या इमरजेंसी में 112 पर कॉल कर सकते हैं। अगर आप तुरंत खतरे में हैं तो 112 पर कॉल करें या नज़दीकी अस्पताल जाएँ, और किसी भरोसेमंद व्यक्ति के साथ रहें। मैं भी आपसे बात करने के लिए यहाँ हूँ।",
    }
}

#[derive(Serialize, Debug)]
pub struct Triage {
    pub level: &'static str, // emergency | urgent | see_doctor_soon | self_care
    pub reasons: Vec<String>,
    pub advice: &'static str,
}

/// Rule-based triage used as an agent tool. Conservative: errs toward escalating.
pub fn triage(symptoms: &str, severity: Option<u8>, duration_days: Option<f32>, age: Option<u32>) -> Triage {
    let mut reasons = Vec::new();
    if detect_emergency(symptoms).is_some() {
        reasons.push("Contains emergency warning signs".to_string());
        return Triage { level: "emergency", reasons, advice: "Call 112/108 or go to the nearest emergency room now." };
    }
    let fever = {
        let s = symptoms.to_lowercase();
        s.contains("fever") || s.contains("बुखार") || s.contains("bukhar")
    };
    let mut level = 0u8; // 0 self_care, 1 see soon, 2 urgent
    if let Some(sev) = severity {
        if sev >= 8 { level = level.max(2); reasons.push(format!("High severity ({sev}/10)")); }
        else if sev >= 5 { level = level.max(1); reasons.push(format!("Moderate severity ({sev}/10)")); }
    }
    if let Some(d) = duration_days {
        if d >= 7.0 { level = level.max(1); reasons.push("Symptoms lasting a week or more".into()); }
    }
    if let Some(a) = age {
        if a <= 1 && fever { level = level.max(2); reasons.push("Fever in an infant".into()); }
        else if a >= 65 && level >= 1 { level = 2; reasons.push("Older age raises risk".into()); }
    }
    match level {
        2 => Triage { level: "urgent", reasons, advice: "Get medical care today (clinic, urgent care or hospital)." },
        1 => Triage { level: "see_doctor_soon", reasons, advice: "Book a doctor visit within the next few days." },
        _ => Triage { level: "self_care", reasons, advice: "Rest, fluids and monitoring are reasonable; see a doctor if it worsens or lasts longer." },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_english_hindi_and_romanized() {
        assert_eq!(detect_emergency("I have severe Chest Pain"), Some(Emergency::Medical));
        assert_eq!(detect_emergency("मुझे सीने में दर्द हो रहा है"), Some(Emergency::Medical));
        assert_eq!(detect_emergency("seene mein dard hai"), Some(Emergency::Medical));
    }

    #[test]
    fn curly_apostrophe_is_normalised() {
        assert_eq!(detect_emergency("he can’t breathe"), Some(Emergency::Medical));
    }

    #[test]
    fn breathing_phrase_variants() {
        for t in ["I can not breathe", "he is unable to breathe", "she stopped breathing", "I cannot breathe properly"] {
            assert_eq!(detect_emergency(t), Some(Emergency::Medical), "{t}");
        }
    }

    #[test]
    fn self_harm_takes_priority() {
        assert_eq!(detect_emergency("I want to die, chest pain too"), Some(Emergency::SelfHarm));
    }

    #[test]
    fn normal_text_is_not_flagged() {
        assert_eq!(detect_emergency("I have a mild headache since morning"), None);
    }

    #[test]
    fn triage_levels() {
        assert_eq!(triage("mild cold", Some(2), Some(2.0), Some(30)).level, "self_care");
        assert_eq!(triage("cough", Some(6), Some(3.0), Some(30)).level, "see_doctor_soon");
        assert_eq!(triage("pain", Some(9), Some(1.0), Some(30)).level, "urgent");
        assert_eq!(triage("fever", None, Some(1.0), Some(0)).level, "urgent");
        assert_eq!(triage("cough", None, Some(10.0), Some(70)).level, "urgent");
        assert_eq!(triage("crushing chest pain", None, None, None).level, "emergency");
    }
}
