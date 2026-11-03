//! Drives the credential provider the way LogonUI does, for testing without a sign-in screen:
//! `harness.exe <path to acceptrequest_credprov.dll> [admin password]`.

#[cfg(windows)]
mod harness {
    use acceptrequest_credprov::CLSID_STR;
    use std::ffi::c_void;
    use windows::Win32::Foundation::{HWND, HMODULE};
    use windows::Win32::Graphics::Gdi::HBITMAP;
    use windows::Win32::System::Com::IClassFactory;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::UI::Shell::*;
    use windows::core::{BOOL, GUID, HRESULT, Interface, PCWSTR, PWSTR, Ref, Result, implement, s};

    #[implement(ICredentialProviderCredentialEvents)]
    struct Events;

    impl ICredentialProviderCredentialEvents_Impl for Events_Impl {
        fn SetFieldState(&self, _: Ref<'_, ICredentialProviderCredential>, _: u32, _: CREDENTIAL_PROVIDER_FIELD_STATE) -> Result<()> { Ok(()) }
        fn SetFieldInteractiveState(&self, _: Ref<'_, ICredentialProviderCredential>, _: u32, _: CREDENTIAL_PROVIDER_FIELD_INTERACTIVE_STATE) -> Result<()> { Ok(()) }
        fn SetFieldString(&self, _: Ref<'_, ICredentialProviderCredential>, id: u32, s: &PCWSTR) -> Result<()> {
            println!("  tile field {id} <- {:?}", unsafe { s.to_string() }.unwrap_or_default());
            Ok(())
        }
        fn SetFieldCheckbox(&self, _: Ref<'_, ICredentialProviderCredential>, _: u32, _: BOOL, _: &PCWSTR) -> Result<()> { Ok(()) }
        fn SetFieldBitmap(&self, _: Ref<'_, ICredentialProviderCredential>, _: u32, _: HBITMAP) -> Result<()> { Ok(()) }
        fn SetFieldComboBoxSelectedItem(&self, _: Ref<'_, ICredentialProviderCredential>, _: u32, _: u32) -> Result<()> { Ok(()) }
        fn DeleteFieldComboBoxItem(&self, _: Ref<'_, ICredentialProviderCredential>, _: u32, _: u32) -> Result<()> { Ok(()) }
        fn AppendFieldComboBoxItem(&self, _: Ref<'_, ICredentialProviderCredential>, _: u32, _: &PCWSTR) -> Result<()> { Ok(()) }
        fn SetFieldSubmitButton(&self, _: Ref<'_, ICredentialProviderCredential>, _: u32, _: u32) -> Result<()> { Ok(()) }
        fn OnCreatingWindow(&self) -> Result<HWND> { Ok(HWND::default()) }
    }

    fn take(p: PWSTR) -> String {
        let s = unsafe { p.to_string() }.unwrap_or_default();
        unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _)) };
        s
    }

    type GetClassObject = unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;

    pub fn main() -> Result<()> {
        let args: Vec<String> = std::env::args().collect();
        let dll: Vec<u16> = args[1].encode_utf16().chain(Some(0)).collect();
        let module: HMODULE = unsafe { LoadLibraryW(PCWSTR(dll.as_ptr())) }?;
        let entry = unsafe { GetProcAddress(module, s!("DllGetClassObject")) }.expect("DllGetClassObject export");
        let get: GetClassObject = unsafe { std::mem::transmute(entry) };
        let clsid = GUID::try_from(CLSID_STR.trim_matches(|c| c == '{' || c == '}')).unwrap();
        let mut factory: *mut c_void = std::ptr::null_mut();
        unsafe { get(&clsid, &IClassFactory::IID, &mut factory) }.ok()?;
        let factory = unsafe { IClassFactory::from_raw(factory) };
        let provider: ICredentialProvider = unsafe { factory.CreateInstance(None) }?;

        unsafe { provider.SetUsageScenario(CPUS_LOGON, 0) }?;
        let n = unsafe { provider.GetFieldDescriptorCount() }?;
        println!("{n} fields:");
        for i in 0..n {
            let d = unsafe { provider.GetFieldDescriptorAt(i) }?;
            let d = unsafe { &*d };
            println!("  {i}: type {} {:?}", d.cpft.0, take(d.pszLabel));
        }
        let (mut count, mut default, mut auto) = (0u32, 0u32, BOOL::default());
        unsafe { provider.GetCredentialCount(&mut count, &mut default, &mut auto) }?;
        println!("tiles: {count}");
        if count == 0 {
            return Ok(());
        }
        let cred = unsafe { provider.GetCredentialAt(0) }?;
        let events: ICredentialProviderCredentialEvents = Events.into();
        unsafe { cred.Advise(&events) }?;
        let _ = unsafe { cred.SetSelected() }?;
        println!("status: {:?}", take(unsafe { cred.GetStringValue(2) }?));
        let bitmap = unsafe { cred.GetBitmapValue(0) };
        println!("tile image: {}", if bitmap.is_ok() { "ok" } else { "missing" });
        let (mut items, mut sel) = (0u32, 0u32);
        unsafe { cred.GetComboBoxValueCount(10, &mut items, &mut sel) }?;
        let amounts: Vec<String> = (0..items).map(|i| take(unsafe { cred.GetComboBoxValueAt(10, i) }.unwrap())).collect();
        println!("parent amounts: {amounts:?}, selected {sel}");

        println!("click 'Ask for 15 minutes more':");
        unsafe { cred.CommandLinkClicked(4) }?;
        println!("click it again:");
        unsafe { cred.CommandLinkClicked(4) }?;

        for (admin, password) in [("", ""), (args.get(2).map_or("nobody", |s| s.as_str()), args.get(3).map_or("wrong", |s| s.as_str()))] {
            println!("submit 'My parent is here' as {admin:?}:");
            let a: Vec<u16> = admin.encode_utf16().chain(Some(0)).collect();
            let p: Vec<u16> = password.encode_utf16().chain(Some(0)).collect();
            unsafe {
                cred.SetStringValue(8, PCWSTR(a.as_ptr()))?;
                cred.SetStringValue(9, PCWSTR(p.as_ptr()))?;
            }
            let mut resp = CREDENTIAL_PROVIDER_GET_SERIALIZATION_RESPONSE::default();
            let mut ser = CREDENTIAL_PROVIDER_CREDENTIAL_SERIALIZATION::default();
            let mut text = PWSTR::null();
            let mut icon = CREDENTIAL_PROVIDER_STATUS_ICON::default();
            unsafe { cred.GetSerialization(&mut resp, &mut ser, &mut text, &mut icon) }?;
            println!("  -> response {} icon {}: {:?}", resp.0, icon.0, take(text));
        }
        println!("bad field id is refused: {}", unsafe { cred.GetStringValue(99) }.is_err());
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    harness::main()
}

#[cfg(not(windows))]
fn main() {}
