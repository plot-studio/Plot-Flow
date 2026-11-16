//! Prompt text and transcript chunking for the local meeting summarizer.
//!
//! Everything here is pure (no I/O) so it can be unit-tested.

/// System prompt for summarizing a whole meeting (or one chunk of it).
/// Tuned on Qwen3.5-4B (Q4_K_M) with Russian ASR transcripts.
pub const SYSTEM_PROMPT: &str = "\
Ты — внимательный секретарь встречи. Тебе дают расшифровку разговора, сделанную распознаванием речи: в ней бывают ошибки в словах, повторы и слова-паразиты («ну», «э-э», «как бы»). Реплики помечены «Я:» (владелец записи) и «Собеседник:» (если собеседников несколько — «Собеседник 1:», «Собеседник 2:» и т. д.; это разные люди).

Составь короткое резюме встречи на русском языке в формате Markdown строго по шаблону:

## Главное
- 2–5 пунктов: какие темы обсудили и к чему пришли, с ключевыми цифрами.

## Решения
- что договорились делать или не делать окончательно.

## Задачи (кто → что → срок)
- Кто → что сделать → срок.

## Открытые вопросы
- что осталось нерешённым, ждёт чьего-то ответа или требует уточнения.

Правила:
- Только факты из расшифровки. Ничего не придумывай и не додумывай.
- Учитывай весь разговор от начала до конца, включая короткие темы вне повестки.
- Не смешивай темы: детали одной темы не переноси в другую.
- Если дата или итог зависят от условия, укажи условие («1 декабря, если выберут виджет, иначе 8 декабря»).
- Предложения, которые только обсуждали, но не приняли, — не решения, их место в «Открытых вопросах».
- Задачи — это явные обещания («я сделаю», «запиши на меня», «к среде») и итоговый список задач, если его проговорили. Не пропускай ни одной.
- Владельца записи пиши «Я», собеседника — по имени, если оно прозвучало, иначе так, как он помечен в расшифровке («Собеседник», «Собеседник 2»). Если срок не назван, пиши «срок не назван».
- Числа, суммы, даты, имена и названия переноси точно; явные ошибки распознавания исправляй по смыслу.
- Каждый пункт — одна короткая строка, до 20 слов. Без вступления и без заключения.
- Раздел, для которого нечего написать, пропусти целиком вместе с заголовком.";

/// System prompt for merging partial summaries of a long meeting.
pub const MERGE_PROMPT: &str = "\
Ты — секретарь встречи. Длинную встречу разбили на части и по каждой уже составили резюме. Объедини их в одно короткое резюме всей встречи на русском языке в формате Markdown строго по шаблону:

## Главное
- 2–6 пунктов: о чём говорили и к чему пришли.

## Решения
- что решили окончательно.

## Задачи (кто → что → срок)
- Кто → что сделать → срок.

## Открытые вопросы
- что осталось нерешённым или требует уточнения.

Правила:
- Только факты из частичных резюме. Ничего не придумывай.
- Убери повторы. Если вопрос из одной части решён в другой, он — решение, а не открытый вопрос.
- Каждый пункт — одна короткая строка, до 20 слов. Без вступления и без заключения.
- Числа, суммы, даты, имена и названия переноси точно.
- Раздел, для которого нечего написать, пропусти целиком вместе с заголовком.";

/// Context window the server is started with (tokens).
pub const CONTEXT_TOKENS: usize = 8192;
/// Hidden "thinking" budget on GPU (server flag `--reasoning-budget`).
/// Thinking noticeably improves accuracy (fewer mixed-up topics, missed
/// tasks) at ~50 tok/s on a GTX 1660 SUPER; on CPU it is switched off.
pub const REASONING_BUDGET_TOKENS: usize = 2048;
/// Tokens reserved for the visible answer.
pub const ANSWER_TOKENS: usize = 900;
/// Tokens reserved for the answer, thinking included.
pub const MAX_ANSWER_TOKENS: usize = REASONING_BUDGET_TOKENS + ANSWER_TOKENS;
/// Tokens reserved for the system prompt and chat template overhead.
const PROMPT_OVERHEAD_TOKENS: usize = 750;
/// Conservative characters-per-token for Russian text with the Qwen3.5
/// tokenizer (measured ≈3.4 chars/token on an ASR transcript).
const CHARS_PER_TOKEN: f32 = 2.8;

/// Max transcript characters that fit into one request.
pub fn max_chunk_chars() -> usize {
    let tokens = CONTEXT_TOKENS - MAX_ANSWER_TOKENS - PROMPT_OVERHEAD_TOKENS;
    (tokens as f32 * CHARS_PER_TOKEN) as usize
}

/// User message for one transcript (or one part of it).
pub fn transcript_message(transcript: &str, part: Option<(usize, usize)>) -> String {
    match part {
        Some((i, n)) => format!(
            "Расшифровка встречи, часть {} из {}:\n\n{}",
            i, n, transcript
        ),
        None => format!("Расшифровка встречи:\n\n{}", transcript),
    }
}

/// User message for the merge step.
pub fn merge_message(partials: &[String]) -> String {
    let mut out = String::from("Резюме частей встречи по порядку:\n");
    for (i, p) in partials.iter().enumerate() {
        out.push_str(&format!("\n### Часть {}\n{}\n", i + 1, p.trim()));
    }
    out
}

/// Groups partial summaries so each group's merge message fits `max_chars`.
/// Every group has at least two items when possible, so repeated merging
/// always makes progress.
pub fn group_for_merge(partials: &[String], max_chars: usize) -> Vec<Vec<String>> {
    if merge_message(partials).chars().count() <= max_chars {
        return vec![partials.to_vec()];
    }
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for p in partials {
        let mut candidate = current.clone();
        candidate.push(p.clone());
        if current.len() >= 2 && merge_message(&candidate).chars().count() > max_chars {
            groups.push(std::mem::take(&mut current));
        }
        current.push(p.clone());
    }
    if !current.is_empty() {
        // Never leave a lone tail: fold it into the previous group.
        if current.len() == 1 && !groups.is_empty() {
            groups.last_mut().unwrap().extend(current);
        } else {
            groups.push(current);
        }
    }
    groups
}

/// Normalizes whitespace: trims lines and drops blank-line runs, which ASR
/// output and joined segments tend to have plenty of.
pub fn normalize_transcript(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Splits a transcript into chunks of at most `max_chars` characters,
/// breaking on line boundaries (speaker turns) when possible, then on
/// sentence ends, then on spaces, and only as a last resort mid-word.
pub fn split_transcript(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for line in text.lines() {
        let line_len = line.chars().count();
        if line_len > max_chars {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                current_len = 0;
            }
            chunks.extend(split_long_line(line, max_chars));
            continue;
        }
        let extra = if current.is_empty() {
            line_len
        } else {
            line_len + 1
        };
        if current_len + extra > max_chars && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            current_len = 0;
        }
        if !current.is_empty() {
            current.push('\n');
            current_len += 1;
        }
        current.push_str(line);
        current_len += line_len;
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks.retain(|c| !c.trim().is_empty());
    chunks
}

fn split_long_line(line: &str, max_chars: usize) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let hard_end = (start + max_chars).min(chars.len());
        let mut end = hard_end;
        if hard_end < chars.len() {
            let window = &chars[start..hard_end];
            let min_cut = window.len() / 2;
            if let Some(p) = window
                .iter()
                .rposition(|c| matches!(c, '.' | '!' | '?' | '…'))
                .filter(|&p| p >= min_cut)
            {
                end = start + p + 1;
            } else if let Some(p) = window
                .iter()
                .rposition(|c| c.is_whitespace())
                .filter(|&p| p >= min_cut)
            {
                end = start + p + 1;
            }
        }
        let piece: String = chars[start..end].iter().collect();
        let piece = piece.trim().to_string();
        if !piece.is_empty() {
            out.push(piece);
        }
        start = end;
    }
    out
}

/// Cleans the model answer: strips a leaked `<think>…</think>` block, code
/// fences around the whole answer and surrounding whitespace.
pub fn clean_answer(raw: &str) -> String {
    let mut text = raw.trim();
    if let Some(pos) = text.find("</think>") {
        text = text[pos + "</think>".len()..].trim();
    }
    let text = text
        .strip_prefix("```markdown")
        .or_else(|| text.strip_prefix("```md"))
        .or_else(|| text.strip_prefix("```"))
        .map(|t| t.strip_suffix("```").unwrap_or(t))
        .unwrap_or(text);
    text.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_transcript_is_one_chunk() {
        let t = "Я: привет\nСобеседник: привет";
        assert_eq!(split_transcript(t, 1000), vec![t.to_string()]);
    }

    #[test]
    fn chunks_respect_limit_and_line_boundaries() {
        let line = "Я: ".to_string() + &"слово ".repeat(20);
        let text = vec![line.trim().to_string(); 10].join("\n");
        let max = line.chars().count() * 3;
        let chunks = split_transcript(&text, max);
        assert!(chunks.len() >= 4);
        for c in &chunks {
            assert!(c.chars().count() <= max, "chunk too long");
            assert!(c.starts_with("Я: "), "chunk must start at a turn");
        }
        let rejoined = chunks.join("\n");
        assert_eq!(rejoined, text);
    }

    #[test]
    fn very_long_line_is_split_on_sentences() {
        let sentence = "Это одно предложение про бюджет. ";
        let line = sentence.repeat(50);
        let chunks = split_transcript(&line, 200);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.chars().count() <= 200);
            assert!(c.ends_with('.'), "split at sentence end: {c:?}");
        }
    }

    #[test]
    fn unbreakable_text_is_hard_split() {
        let s = "а".repeat(450);
        let chunks = split_transcript(&s, 200);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.concat(), s);
    }

    #[test]
    fn empty_transcript_has_no_chunks() {
        assert!(split_transcript("  \n \n", 100).is_empty());
    }

    #[test]
    fn chunk_budget_fits_context() {
        let tokens = (max_chunk_chars() as f32 / CHARS_PER_TOKEN) as usize;
        assert!(tokens + MAX_ANSWER_TOKENS + PROMPT_OVERHEAD_TOKENS <= CONTEXT_TOKENS);
        assert!(max_chunk_chars() > 10_000);
    }

    #[test]
    fn normalize_drops_blank_lines() {
        assert_eq!(normalize_transcript("  a \n\n\n b\n"), "a\nb");
    }

    #[test]
    fn messages_mention_parts() {
        assert!(transcript_message("x", Some((2, 3))).contains("часть 2 из 3"));
        assert!(transcript_message("x", None).ends_with("x"));
        let m = merge_message(&["- a".into(), "- b".into()]);
        assert!(m.contains("### Часть 1\n- a") && m.contains("### Часть 2\n- b"));
    }

    #[test]
    fn merge_groups_fit_or_make_progress() {
        let parts: Vec<String> = (0..7)
            .map(|i| format!("- пункт {} {}", i, "x".repeat(80)))
            .collect();
        assert_eq!(group_for_merge(&parts, 100_000).len(), 1);
        let groups = group_for_merge(&parts, 300);
        assert!(groups.len() > 1 && groups.len() < parts.len());
        assert!(groups.iter().all(|g| g.len() >= 2));
        assert_eq!(groups.concat(), parts);
    }

    #[test]
    fn clean_answer_strips_think_and_fences() {
        assert_eq!(
            clean_answer("<think>hmm</think>\n## Главное\n- x"),
            "## Главное\n- x"
        );
        assert_eq!(clean_answer("```markdown\n## Главное\n```"), "## Главное");
        assert_eq!(clean_answer("  ## Главное  "), "## Главное");
    }

    #[test]
    fn system_prompt_has_all_sections() {
        for s in [
            "## Главное",
            "## Решения",
            "## Задачи (кто → что → срок)",
            "## Открытые вопросы",
        ] {
            assert!(SYSTEM_PROMPT.contains(s));
            assert!(MERGE_PROMPT.contains(s));
        }
    }
}
