# Plot Flow 2.0 — состояние работ (пауза 5 ноября 2026, ~10:05)

План целиком: `PLAN-Plot-Flow-2.0.md`. Ниже — что сделано, что не проверено и как продолжить.

## Где что
- Рабочая копия (здесь): `C:\Moksem\P3-plot-flow`, ветка `v2`, последний коммит — WIP-снимок этой паузы.
- Оригинальные исходники — не тронуты: `C:\Users\Игорь\Desktop\Handy — Plot Flow\Handy-main` (ветка `main`).
- Оригинальная установленная программа — не тронута: `C:\Users\Игорь\Desktop\Plot Flow`.
- Резервные копии данных (статистика, история, настройки, логи, записи):
  `C:\Moksem\P3-plot-flow\backup\Data_2026-11-05_09-01`, `..._09-31` и `D:\PlotFlow-backup\` (там же бандл git-истории `plot-flow-source_*.bundle`).
- Кэш сборки Rust: `D:\plot-flow-target` (задан в `.cargo\config.toml`, один поток сборки).
- Сборка: `powershell -File scripts\dev-build.ps1` (exe), `-Test` (cargo test), `-Bundle` (установщик; перед ним положить модель GGUF в `src-tauri\resources\models`, см. `scripts\prepare-model.ps1`).

## Готовность по этапам
| Этап | Состояние |
|---|---|
| 0. Подготовка | ✅ готово (копия, бэкапы, кэш на D:, скрипт сборки) |
| 1. Надёжная диктовка | 🟡 код написан, **не собран и не протестирован** |
| — баг «запись сразу сбрасывается» | 🟡 причина найдена и исправлена в коде, не собрано |
| 2. Оболочка (окно, тёмная тема, меню) | 🟡 ~30 %: окно 1000×680 фиксировано, тёмная палитра и схема настроек 4 внесены; сайдбар/страницы не переделаны |
| 3. «Совещание» | 🟠 ~15 %: написаны `meeting/types.rs`, `meeting/store.rs` (не подключены к крейту), TS-биндинги и переводы; захват звука, живой транскрипт, страница — не начаты |
| 4. Голосовые профили | ⬜ |
| 5. «Робот» (резюме через локальную нейросеть) | ⬜ |
| 6. Статистика+ | ⬜ |

Общая оценка: выполнено порядка 20–25 % всего объёма.

## Что именно сделано в коде (этап 1 и баг)
- `src-tauri/src/audio_toolkit/chunking.rs` (новый): `plan_chunks` — резка записи по паузам на куски ≤ окна модели; `boost_quiet_audio` — программное усиление тихих записей; юнит-тесты.
- `managers/transcription.rs`: длинная запись режется (`run_transcribe_cpp_windowed`) по `caps.max_audio_ms` модели (GigaAM ≈ 25 с) и склеивается; тихая запись усиливается при включённом `auto_mic_gain`.
- `audio_toolkit/text.rs`: русский список слов-паразитов («э», «э-э», «ээ», «эм», «а-а», «м-м»…), схлопывание повторов коротких слов («и и» → «и»); тесты.
- `managers/audio.rs`: усиление микрофона до 100 % на время записи через Core Audio (`IAudioEndpointVolume`), восстановление уровня после стопа/отмены; новая настройка `auto_mic_gain` (settings.rs, команда `change_auto_mic_gain_setting`, тумблер `AutoMicGain.tsx`, переводы en/ru, `bindings.ts`, `settingsStore.ts`).
- `Cargo.toml`: добавлены фичи `windows` (`Win32_Media_Audio`, `Win32_Devices_Properties`, `Win32_UI_Shell_PropertiesSystem`, `Win32_System_Com`).
- Баг горячей клавиши (`transcription_coordinator.rs`): время нажатия/отпускания теперь берётся в момент события, а не при обработке из очереди. Раньше открытие микрофона (до 350 мс) задерживало обработку, короткий тап F24 считался удержанием, и отпускание клавиши тут же останавливало запись (0 семплов). Порог удержания по умолчанию 300 → 500 мс.

## Правки агентов (прерваны на паузе)
- Этап 2: `src/App.css`, `src/styles/theme.css` (тёмная палитра), `lib.rs` (фиксированное окно), `settings.rs` (тема Dark по умолчанию + миграция схемы 4). Не сделано: сайдбар с 5 разделами (Диктовка · Совещания · История · Статистика · Настройки), страницы, вёрстка под 1000×680.
- Этап 3 backend: `src-tauri/src/meeting/{types.rs,store.rs}` — модуль **не объявлен** в `lib.rs` (строки `pub mod meeting;` нет), поэтому на сборку не влияет. Нужны ещё `mod.rs`, `capture.rs` (микрофон + WASAPI loopback), `session.rs` (VAD-сегментация, куски WAV по 30 с, транскрипция через `TranscriptionManager`), `export.rs`, `commands.rs` — контракт команд/событий описан в брифе (см. раздел ниже).
- Этап 3 frontend: в `src/bindings.ts` добавлены обёртки `meeting*` и типы; в `translation.json` (en, ru) — ключи `meeting.*`. Компоненты `src/components/meeting/**` не созданы.

## Как продолжить (порядок)
1. `powershell -File scripts\dev-build.ps1 -Test` — собрать и прогнать тесты (первая сборка долгая: пересобираются скрипты сборки зависимостей, в т.ч. transcribe-cpp-sys с ggml/Vulkan, и крейт `windows` с новыми фичами). Исправить ошибки компиляции, если будут (код этапа 1 ещё не компилировался).
2. Проверить резку на реальных записях: `D:\plot-flow-target\release\plot-flow.exe --transcribe-file <wav>` для файлов из `backup\...\recordings\` (там записи по 57–164 с, которые раньше ломались). Сравнить с текстом из `history.db` бэкапа.
3. Доделать этап 2 (сайдбар, страницы, вёрстка) и этап 3 по контракту; подключить `pub mod meeting;`, зарегистрировать команды в `collect_commands!`, `.manage(MeetingManager)`.
4. Собрать установщик (`-Bundle`), ставить в **отдельную** папку (например `D:\Plot Flow 2`), не трогая `Desktop\Plot Flow`; статистику скопировать из `Data`.

## Контракт «Совещания» (для backend и frontend)
Команды: `meeting_start(sources)`, `meeting_stop`, `meeting_pause`, `meeting_resume`, `meeting_discard`, `meeting_status`, `meeting_list`, `meeting_get(meeting_id)`, `meeting_rename(meeting_id,title)`, `meeting_rename_speaker(meeting_id,speaker_id,name)`, `meeting_delete(meeting_id)`, `meeting_export(meeting_id,format)`.
События: `meeting-status` (≈4 раза/с), `meeting-segment`, `meeting-finished`, `meeting-error`.
Типы: `MeetingSources{microphone,system_audio}`, `MeetingState idle|recording|paused|finalizing`, `MeetingStatus`, `MeetingInfo`, `MeetingSpeaker`, `MeetingSegment`, `MeetingDetail`, `MeetingExportFormat txt|md|json|srt` — Rust-версии уже в `meeting/types.rs`, TS-версии — в `bindings.ts`.
Хранение: `<Data>\meetings.db` (WAL), аудио кусками по 30 с в `<Data>\meetings\<id>\{mic,sys}-NNNN.wav`, экспорт в `<Data>\meetings\exports\`.

## Открытые вопросы к владельцу
1. Размер окна 1000×680 — ок? 2. «Робот»: Ollama/LM Studio сначала или сразу встраивать llama.cpp? 3. Хранить ли аудио созвонов (≈60 МБ/час) или только текст?
