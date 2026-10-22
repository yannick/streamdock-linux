// The custom keys editor (H15-29), as reusable pieces: the /keys page uses them, and so can the
// dashboard (H15-32, a dialog on the dock mirror).
export { ActionEditor } from "./ActionEditor.js";
export { ImagePicker } from "./ImagePicker.js";
export { SlotEditor, type SlotEditorProps } from "./SlotEditor.js";
export {
  ACTION_TYPES,
  choicesOf,
  errorText,
  findSlot,
  newAction,
  pageModeId,
  savePages,
  useKeyPages,
  withSlot,
  type Choices,
  type IconRef,
  type SlotRef,
} from "./model.js";
