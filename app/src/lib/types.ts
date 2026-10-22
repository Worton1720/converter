// Типы повторяют serde-представление структур ядра (crates/core) и dto.rs.

export type ImageFormat = "Png" | "Jpeg" | "Webp";
export type VideoMode = "Compat" | "Compact" | { Webm: "Av1" | "Vp9" } | "Mkv";
export type QualityProfile = "Fast" | "Balanced" | "High";
export type HwBackend = "Nvenc" | "Qsv" | "Amf" | "Vaapi";
export type HwPreference = "Auto" | "Off" | { Only: HwBackend };
export type MetadataPolicy = "Keep" | "StripGps" | "StripAll";
export type OutputDir = "BesideSource" | { Into: string } | { IntoKeepTree: string };
export type ConflictPolicy = "Suffix" | "Overwrite" | "Skip";
export type SourcePolicy = "Keep" | "Trash" | "Delete";

export interface ImageOptions {
  format: ImageFormat;
  quality: number;
  png_compression: "Fast" | "Compact";
  max_side: number | null;
  png_16bit: boolean;
  to_srgb: boolean;
  fit_webp_limit: boolean;
}

export interface VideoOptions {
  mode: VideoMode;
  quality: QualityProfile;
  hw: HwPreference;
  audio_bitrate_kbps: number;
}

export interface ConvertOptions {
  image: ImageOptions;
  video: VideoOptions;
  metadata: MetadataPolicy;
  output: OutputDir;
  name_template: string;
  conflict: ConflictPolicy;
  sources: SourcePolicy;
  preserve_mtime: boolean;
  skip_live_photo_video: boolean;
  remove_live_photo_video: boolean;
}

export type LoadLevel =
  | "Auto"
  | "Low"
  | "Medium"
  | "High"
  | "Max"
  | { Custom: { threads: number; low_priority: boolean; hw_sessions: number } };

export type FfmpegSource = "Auto" | "Bundled" | "System" | { Custom: string };
export type KindFilter = "All" | "Images" | "Videos" | { Extensions: string[] };
export type KeepRule = "Oldest" | "Newest" | "ShortestPath" | { PriorityFolders: string[] };
export type DupAction = "Trash" | "Delete" | { MoveTo: string };
export type Theme = "System" | "Light" | "Dark";

export interface Settings {
  convert: ConvertOptions;
  load: LoadLevel;
  ffmpeg: FfmpegSource;
  theme: Theme;
  large_text: boolean;
  limit_on_battery: boolean;
  prevent_sleep: boolean;
  dupes: {
    min_size: number;
    kinds: KindFilter;
    include_hidden: boolean;
    include_cloud: boolean;
    keep: KeepRule;
    exclude: string[];
  };
  presets: { name: string; convert: ConvertOptions }[];
  scan_recursive: boolean;
  scan_hidden: boolean;
}

export interface GovernorStatus {
  level: LoadLevel;
  budget: number;
  threads_total: number;
  external_load: number;
  own_load: number;
  on_battery: boolean;
  memory_low: boolean;
  priority: "Normal" | "BelowNormal" | "Low";
}

export interface AppInfo {
  version: string;
  threads: number;
  ffmpeg: { path: string; version: string; origin: string } | null;
  ffmpeg_error: string | null;
  hw: { backend: string; codec: string; device: string | null }[];
  max_sessions: number;
  settings: Settings;
  settings_path: string;
  log_path: string;
  startup_paths: string[];
  status: GovernorStatus | null;
}

export interface Item {
  path: string;
  name: string;
  kind: "image" | "video";
  format: string;
  size: number;
  width: number | null;
  height: number | null;
  duration: number | null;
  codec: string | null;
  hdr: boolean;
  live: "no" | "video" | "photo";
  error: string | null;
  inspected: boolean;
}

export type SourceAction = "Kept" | "Trashed" | "Deleted" | { KeptBecause: string };

export type JobOutcome =
  | { Done: { output: string; operation: string; source: SourceAction } }
  | { Skipped: { reason: string } }
  | { Failed: { error: string } }
  | "Cancelled";

export interface RunSummary {
  done: number;
  skipped: number;
  failed: number;
  cancelled: number;
}

export type EngineEvent =
  | { JobStarted: { id: number; source: string; operation: string } }
  | { JobProgress: { id: number; fraction: number; speed: number | null } }
  | { JobFinished: { id: number; source: string; outcome: JobOutcome } }
  | { Log: { id: number | null; level: "Info" | "Warn" | "Error"; message: string } }
  | { PausedNoSpace: { path: string } }
  | { Governor: GovernorStatus }
  | { RunFinished: RunSummary };

export interface SpaceCheck {
  mount: string;
  needed: number;
  free: number;
}

export interface DupesOptions {
  roots: string[];
  recursive: boolean;
  include_hidden: boolean;
  include_cloud: boolean;
  min_size: number;
  kinds: KindFilter;
  exclude: string[];
  use_cache: boolean;
}

export interface DupFile {
  path: string;
  size: number;
  mtimeMs: number;
  links: number;
  kind: "image" | "video" | null;
}

export interface DupGroup {
  size: number;
  hash: string;
  files: DupFile[];
}

export interface DupesResult {
  groups: DupGroup[];
  marks: boolean[][];
  scanned: number;
  skippedCloud: number;
  hardlinks: number;
  errors: [string, string][];
}

export type DupesProgress =
  | { Scanning: { found: number } }
  | { Hashing: { stage: number; done: number; total: number; bytes_done: number; bytes_total: number } };

export interface ActionResult {
  path: string;
  error: string | null;
}
