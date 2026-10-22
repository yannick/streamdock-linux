// Picks a key image by name (H15-28): an uploaded picture or a built-in icon (H15-48), grouped.
// Upload new pictures on /settings → Key images; an upload with an icon's name replaces the icon.
import type { IconRef } from "./model.js";

interface ImagePickerProps {
  id?: string;
  images: string[];
  icons?: readonly IconRef[];
  value: string;
  onChange(name: string): void;
  /** Shown for "none". */
  placeholder?: string;
}

export function ImagePicker({ id, images, icons = [], value, onChange, placeholder = "none" }: ImagePickerProps) {
  const known = value === "" || images.includes(value) || icons.some((i) => i.name === value);
  const groups = [...new Set(icons.map((i) => i.group))];
  return (
    <select id={id} className="sd-input" value={value} onChange={(e) => onChange(e.currentTarget.value)}>
      <option value="">{placeholder}</option>
      {!known && <option value={value}>{value} (not uploaded)</option>}
      {images.length > 0 && (
        <optgroup label="Uploaded">
          {images.map((name) => (
            <option key={name}>{name}</option>
          ))}
        </optgroup>
      )}
      {groups.map((group) => (
        <optgroup key={group} label={`Icons: ${group}`}>
          {icons
            .filter((i) => i.group === group && !images.includes(i.name))
            .map((i) => (
              <option key={i.name}>{i.name}</option>
            ))}
        </optgroup>
      ))}
    </select>
  );
}
