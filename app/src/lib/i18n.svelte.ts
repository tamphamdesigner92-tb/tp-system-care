import vi from './locales/vi';
import en from './locales/en';

export type Lang = 'vi' | 'en';
export type Key = keyof typeof vi;

const dicts: Record<Lang, Record<Key, string>> = { vi, en };
const STORAGE_KEY = 'tp.lang';

function initialLang(): Lang {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved === 'vi' || saved === 'en') return saved;
  } catch {
    // localStorage không khả dụng: dùng mặc định.
  }
  return 'vi';
}

export const i18n = $state({ lang: initialLang() });

/** `params` thay cho các chỗ `{tên}` trong chuỗi. */
export function t(key: Key, params?: Record<string, string | number>): string {
  const text = dicts[i18n.lang][key];
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (match, name) => (name in params ? String(params[name]) : match));
}

/** Nhãn cho khóa do backend trả về; không có bản dịch thì trả về `fallback`. */
export function tOr(key: string, fallback: string): string {
  return (dicts[i18n.lang] as Record<string, string>)[key] ?? fallback;
}

export function setLang(lang: Lang): void {
  i18n.lang = lang;
  document.documentElement.lang = lang;
  try {
    localStorage.setItem(STORAGE_KEY, lang);
  } catch {
    // Bỏ qua: lựa chọn chỉ không được nhớ cho lần sau.
  }
}
