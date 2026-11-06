/*
 * Copyright (C) 2011-2026 MicroSIP (http://www.microsip.org)
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 */

#include "stdafx.h"
#include "settings.h"
#include "Crypto.h"

#include <algorithm>
#include <vector>

Statistics statistics = {};
AccountSettings accountSettings;
int dpiY;
bool firstRun;
CTime startTime;
CArray<Shortcut, Shortcut> shortcuts;

static LONGLONG FileSize(const wchar_t* name)
{
    WIN32_FILE_ATTRIBUTE_DATA fad;
    if (!GetFileAttributesEx(name, GetFileExInfoStandard, &fad))
        return -1; // error condition, could call GetLastError to find out more
    LARGE_INTEGER size;
    size.HighPart = fad.nFileSizeHigh;
    size.LowPart = fad.nFileSizeLow;
    return size.QuadPart;
}

bool IniDecrypt(CString& str)
{
    if (!str.IsEmpty()) {
        bool res = false;
        CByteArray arPassword;
        MSIP::String2Bin(str, &arPassword);
        MFC::CCrypto crypto;
        CString key = (LPCTSTR)_GLOBAL_KEY;
        if (crypto.DeriveKey(key)) {
            try {
                if (crypto.Decrypt(arPassword, str)) {
                    res = true;
                }
            }
            catch (CArchiveException* e) {
            }
        }
        return res;
    }
    else {
        return true;
    }
}
CString IniEncrypt(CString str)
{
    CString res;
    MFC::CCrypto crypto;
    CByteArray arPassword;
    CString key = (LPCTSTR)_GLOBAL_KEY;
    if (!str.IsEmpty() && crypto.DeriveKey(key)
        && crypto.Encrypt(str, arPassword)
        ) {
        res = MSIP::Bin2String(&arPassword);
    }
    else {
        res = str;
    }
    return res;
}

void AccountSettings::Init()
{
    isPortable = false;
    firstRun = false;
    CString str;
    LPTSTR ptr;
    accountId = 0;
    startTime = CTime::GetCurrentTime();
    //--
    ptr = exeFile.GetBuffer(MAX_PATH);
    GetModuleFileName(NULL, ptr, MAX_PATH);
    exeFile.ReleaseBuffer();
    //--
    pathExe = exeFile.Mid(0, exeFile.ReverseFind('\\'));
    //--
    CString fileName = PathFindFileName(exeFile);
    fileName = fileName.Mid(0, fileName.ReverseFind('.'));
    CString version = _T(_GLOBAL_VERSION);
    logFile.Format(_T("%s_log.txt"), fileName);
    iniFile.Format(_T("%s.ini"), fileName);
    pathRoaming = _T("");
    pathLocal = _T("");

    ptr = appDataRoamingRoot.GetBuffer(MAX_PATH);
    SHGetSpecialFolderPath(
        0,
        ptr,
        CSIDL_APPDATA,
        FALSE);
    appDataRoamingRoot.ReleaseBuffer();
    appDataRoaming = appDataRoamingRoot;
    appDataRoaming.AppendFormat(_T("\\%s\\"), _T(_GLOBAL_NAME_NICE));

    ptr = appDataLocalRoot.GetBuffer(MAX_PATH);
    SHGetSpecialFolderPath(
        0,
        ptr,
        CSIDL_LOCAL_APPDATA,
        FALSE);
    appDataLocalRoot.ReleaseBuffer();
    appDataLocal = appDataLocalRoot;
    appDataLocal.AppendFormat(_T("\\%s\\"), _T(_GLOBAL_NAME_NICE));
    if (pathRoaming.IsEmpty()) {
        CString contactsFile = _T("Contacts.xml");
        CString pathInstaller;
        CRegKey regKey;
        CString rab;
        ULONG pnChars;
        rab.Format(_T("Software\\%s"), _T(_GLOBAL_NAME_NICE));
        if (regKey.Open(HKEY_CURRENT_USER, rab, KEY_READ) == ERROR_SUCCESS) {
            ptr = pathInstaller.GetBuffer(256);
            pnChars = 256;
            regKey.QueryStringValue(NULL, ptr, &pnChars);
            pathInstaller.ReleaseBuffer();
            regKey.Close();
        }
        if (pathInstaller.IsEmpty() && regKey.Open(HKEY_LOCAL_MACHINE, rab, KEY_READ) == ERROR_SUCCESS) {
            ptr = pathInstaller.GetBuffer(256);
            pnChars = 256;
            regKey.QueryStringValue(NULL, ptr, &pnChars);
            pathInstaller.ReleaseBuffer();
            regKey.Close();
        }
        if (!pathInstaller.IsEmpty() && pathInstaller.CompareNoCase(pathExe) == 0) {
            // installer
            CreateDirectory(appDataRoaming, NULL);
            pathRoaming = appDataRoaming;
            CreateDirectory(appDataLocal, NULL);
            pathLocal = appDataLocal;
            logFile = pathLocal + logFile;
            if (!::PathFileExists(pathRoaming + iniFile) && ::PathFileExists(pathLocal + iniFile)) {
                MoveFile(pathLocal + iniFile, pathRoaming + iniFile);
            }
            if (!::PathFileExists(pathRoaming + contactsFile) && ::PathFileExists(pathLocal + contactsFile)) {
                MoveFile(pathLocal + contactsFile, pathRoaming + contactsFile);
            }
        }
        else {
            // portable
            isPortable = true;
            pathRoaming = pathExe + _T("\\");
            pathLocal = pathRoaming;
            logFile = pathLocal + logFile;
        }
        iniFile = pathRoaming + iniFile;
        if (!::PathFileExists(iniFile)) {
            CString name = _T(_GLOBAL_NAME_NICE);
            if (name.Find(' ') != -1) {
                CString iniFileNice = pathRoaming;
                iniFileNice.AppendFormat(_T("%s.ini"), _T(_GLOBAL_NAME_NICE));
                if (::PathFileExists(iniFileNice)) {
                    MoveFile(iniFileNice, iniFile);
                }
            }
        }
        if (lstrcmp(AfxGetApp()->m_lpCmdLine, _T("/reset")) == 0) {
            return;
        }
        bool iniFailed = false;
        if (!::PathFileExists(iniFile) || FileSize(iniFile) == 0) {
            firstRun = true;
            // create UTF16-LE BOM(FFFE)
            WORD wBOM = 0xFEFF;
            CString pszSectionB = _T("[Global]");
            CFile file;
            CFileException fileException;
            if (file.Open(iniFile, CFile::modeCreate | CFile::modeReadWrite | CFile::typeBinary | CFile::shareExclusive, &fileException)) {
                file.Write(&wBOM, sizeof(wBOM));
                file.Write(pszSectionB.GetString(), pszSectionB.GetLength() * sizeof(wchar_t));
                file.Close();
            }
            else {
                iniFailed = true;
            }
        }
        else if (!firstRun) {
            CFile file;
            CFileException fileException;
            if (file.Open(iniFile, CFile::modeReadWrite | CFile::typeBinary | CFile::shareExclusive, &fileException)) {
                WORD wBOM;
                if (sizeof(WORD) == file.Read(&wBOM, sizeof(WORD))) {
                    if (wBOM != 0xFEFF) {
                        // convert to UTF16-LE BOM
                        file.SeekToBegin();
                        CStringA data;
                        int i;
                        UINT len = 0;
                        do {
                            LPSTR p = data.GetBuffer(len + 1024);
                            i = file.Read(p + len, 1024);
                            len += i;
                            data.ReleaseBuffer(len);
                        } while (i > 0);
                        file.SetLength(0);
                        wBOM = 0xFEFF;
                        file.Write(&wBOM, sizeof(wBOM));
                        CString res = MSIP::AnsiToWideChar(data.GetString());
                        file.Write(res.GetString(), data.GetLength() * sizeof(wchar_t));
                    }
                }
                file.Close();
            }
            else {
                iniFailed = true;
            }
        }
        if (iniFailed) {
            CString str;
            str.Format(_T("Failed to open file for writing %s"), iniFile);
            throw std::runtime_error(CW2A(str, CP_UTF8));
        }
        if (firstRun) {
            if (!isPortable) {
                DWORD runAtSystemStartup = -1;
                CRegKey regKey;
                CString rab;
                rab.Format(_T("Software\\%s"), _T(_GLOBAL_NAME_NICE));
                if (regKey.Open(HKEY_CURRENT_USER, rab, KEY_READ) == ERROR_SUCCESS) {
                    regKey.QueryDWORDValue(_T("RunAtSystemStartup"), runAtSystemStartup);
                    regKey.Close();
                }
                if (!runAtSystemStartup && regKey.Open(HKEY_LOCAL_MACHINE, rab, KEY_READ) == ERROR_SUCCESS) {
                    regKey.QueryDWORDValue(_T("RunAtSystemStartup"), runAtSystemStartup);
                    regKey.Close();
                }
                msip_startup_set(runAtSystemStartup != 0);
            }
        }
    }
    //--

    CString section;
    CString sectionSettings = _T("Settings");

    //--
    section = _T("Global");

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("version"), NULL, ptr, 256, iniFile);
    str.ReleaseBuffer();
    if (str != version) {
        WritePrivateProfileString(section, _T("version"), version, iniFile);
        WritePrivateProfileString(sectionSettings, _T("version"), NULL, iniFile);
    }

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("checkUpdatesTime"), NULL, ptr, 256, iniFile);
    str.ReleaseBuffer();
    if (str.IsEmpty()) {
        WritePrivateProfileString(sectionSettings, _T("checkUpdatesTime"), NULL, iniFile);
    }
    checkUpdatesTime = _wtoi(str);

    //--
    SettingsLoad();
    //--
    ptr = str.GetBuffer(256);
    GetPrivateProfileString(sectionSettings, _T("accountId"), NULL, ptr, 256, iniFile);
    str.ReleaseBuffer();
    if (str.IsEmpty()) {
        if (AccountLoad(-2, &account)) {
            accountId = 1;
            WritePrivateProfileString(sectionSettings, _T("accountId"), _T("1"), iniFile);
        }
    }
    else {
        accountId = _wtoi(str);
        if (!accountId && !enableLocalAccount) {
            accountId = 1;
        }
        if (accountId > 0) {
            if (!AccountLoad(accountId, &account)) {
                accountId = 0;
            }
        }
    }
    AccountLoad(0, &accountLocal);
}

AccountSettings::AccountSettings()
{
}

void AccountSettings::AccountDelete(int id)
{
    CString section;
    section.Format(_T("Account%d"), id);
    WritePrivateProfileString(section, NULL, NULL, iniFile);
}

bool AccountSettings::AccountLoad(int id, Account* account, CString filename)
{
    if (filename.IsEmpty()) {
        filename = iniFile;
    }

    CString str;
    CString rab;
    LPTSTR ptr;

    CString section;
    if (id == -2) {
        section = _T("Settings");
    }
    else {
        section.Format(_T("Account%d"), id);
    }

    bool sectionExists = MSIP::IniSectionExists(section, filename);

    ptr = account->label.GetBuffer(256);
    GetPrivateProfileString(section, _T("label"), id ? NULL : _T("Local (call by IP address)"), ptr, 256, filename);
    account->label.ReleaseBuffer();

    CString iniFileDefault = filename;

    ptr = account->server.GetBuffer(1040);
    GetPrivateProfileString(section, _T("server"), NULL, ptr, 1041, filename);
    account->server.ReleaseBuffer();
    ptr = account->proxy.GetBuffer(1040);
    GetPrivateProfileString(section, _T("proxy"), NULL, ptr, 1041, filename);
    account->proxy.ReleaseBuffer();

    ptr = account->domain.GetBuffer(1040);
    GetPrivateProfileString(section, _T("domain"), NULL, ptr, 1041, filename);
    account->domain.ReleaseBuffer();

    ptr = account->username.GetBuffer(1040);
    GetPrivateProfileString(section, _T("username"), NULL, ptr, 1041, filename);
    account->username.ReleaseBuffer();

    ptr = account->password.GetBuffer(1040);
    GetPrivateProfileString(section, _T("password"), NULL, ptr, 1041, filename);
    account->password.ReleaseBuffer();
    if (!account->password.IsEmpty() && !IniDecrypt(account->password)) {
        WritePrivateProfileString(section, _T("password"), IniEncrypt(account->password), filename);
    }

    account->rememberPassword = account->username.GetLength() ? 1 : 0;


    ptr = account->authID.GetBuffer(1040);
    GetPrivateProfileString(section, _T("authID"), NULL, ptr, 1041, filename);
    account->authID.ReleaseBuffer();

    ptr = account->displayName.GetBuffer(1040);
    GetPrivateProfileString(section, _T("displayName"), NULL, ptr, 1041, filename);
    account->displayName.ReleaseBuffer();

    ptr = account->dialingPrefix.GetBuffer(256);
    GetPrivateProfileString(section, _T("dialingPrefix"), NULL, ptr, 256, filename);
    account->dialingPrefix.ReleaseBuffer();

    ptr = account->dialPlan.GetBuffer(256);
    GetPrivateProfileString(section, _T("dialPlan"), NULL, ptr, 256, filename);
    account->dialPlan.ReleaseBuffer();

    ptr = account->hideCID.GetBuffer(256);
    GetPrivateProfileString(section, _T("hideCID"), NULL, ptr, 256, filename);
    account->hideCID.ReleaseBuffer();
    if (account->hideCID == _T("0")) {
        account->hideCID.Empty();
    }
    if (account->hideCID == _T("1")) {
        account->hideCID = _T("hard");
    }

    ptr = account->voicemailNumber.GetBuffer(256);
    GetPrivateProfileString(section, _T("voicemailNumber"), NULL, ptr, 256, filename);
    account->voicemailNumber.ReleaseBuffer();

    ptr = account->srtp.GetBuffer(256);
    GetPrivateProfileString(section, _T("SRTP"), NULL, ptr, 256, filename);
    account->srtp.ReleaseBuffer();

    ptr = account->transport.GetBuffer(256);
    GetPrivateProfileString(section, _T("transport"), _T("udp"), ptr, 256, filename);
    account->transport.ReleaseBuffer();

    ptr = account->publicAddr.GetBuffer(256);
    GetPrivateProfileString(section, _T("publicAddr"), NULL, ptr, 256, filename);
    account->publicAddr.ReleaseBuffer();

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("registerRefresh"), _T("300"), ptr, 256, filename);
    str.ReleaseBuffer();
    account->registerRefresh = _wtoi(str);
    if (account->registerRefresh <= 0) {
        account->registerRefresh = 300;
    }
    else if (account->registerRefresh <= 10) {
        account->registerRefresh = 10;
    }
    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("keepAlive"), _T("15"), ptr, 256, filename);
    str.ReleaseBuffer();
    account->keepAlive = _wtoi(str);
    if (account->keepAlive <= 0) {
        account->keepAlive = 15;
    }
    else if (account->keepAlive == 1) {
        account->keepAlive = 2;
    }

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("publish"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    account->publish = str == _T("1") ? 1 : 0;

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("allowRewrite"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    account->allowRewrite = str == _T("1") ? 1 : 0;

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("ICE"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    account->ice = str == _T("1") ? 1 : 0;

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("disableSessionTimer"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    account->disableSessionTimer = str == _T("1") ? 1 : 0;
    if (id == -2) {
        // delete old
        WritePrivateProfileString(section, _T("server"), NULL, filename);
        WritePrivateProfileString(section, _T("proxy"), NULL, filename);
        WritePrivateProfileString(section, _T("SRTP"), NULL, filename);
        WritePrivateProfileString(section, _T("transport"), NULL, filename);
        WritePrivateProfileString(section, _T("publicAddr"), NULL, filename);
        WritePrivateProfileString(section, _T("publish"), NULL, filename);
        WritePrivateProfileString(section, _T("STUN"), NULL, filename);
        WritePrivateProfileString(section, _T("ICE"), NULL, filename);
        WritePrivateProfileString(section, _T("allowRewrite"), NULL, filename);
        WritePrivateProfileString(section, _T("domain"), NULL, filename);
        WritePrivateProfileString(section, _T("authID"), NULL, filename);
        WritePrivateProfileString(section, _T("username"), NULL, filename);
        WritePrivateProfileString(section, _T("passwordSize"), NULL, filename);
        WritePrivateProfileString(section, _T("password"), NULL, filename);
        WritePrivateProfileString(section, _T("id"), NULL, filename);
        WritePrivateProfileString(section, _T("displayName"), NULL, filename);
        // save new
        //if (!account->domain.IsEmpty() && !account->username.IsEmpty()) {
        if (sectionExists && !account->domain.IsEmpty()) {
            AccountSave(1, account);
        }
    }
    //return !account->domain.IsEmpty() && !account->username.IsEmpty();

    if (id == 0) {
        return sectionExists;// local account
    }

    return  sectionExists && !account->domain.IsEmpty();
}

void AccountSettings::AccountSave(int id, Account* account, CString filename)
{
    if (filename.IsEmpty()) {
        filename = iniFile;
    }

    CString str;
    CString section;
    section.Format(_T("Account%d"), id);

    WritePrivateProfileString(section, _T("label"), account->label, filename);

    WritePrivateProfileString(section, _T("server"), account->server, filename);

    WritePrivateProfileString(section, _T("proxy"), account->proxy, filename);

    WritePrivateProfileString(section, _T("domain"), account->domain, filename);

    if (!account->rememberPassword) {
        WritePrivateProfileString(section, _T("username"), _T(""), filename);
        WritePrivateProfileString(section, _T("password"), _T(""), filename);
    }
    else {
        WritePrivateProfileString(section, _T("username"), account->username, filename);
        WritePrivateProfileString(section, _T("password"), IniEncrypt(account->password), filename);
    }

    WritePrivateProfileString(section, _T("authID"), account->authID, filename);

    WritePrivateProfileString(section, _T("displayName"), account->displayName, filename);

    WritePrivateProfileString(section, _T("dialingPrefix"), account->dialingPrefix, filename);

    WritePrivateProfileString(section, _T("dialPlan"), account->dialPlan, filename);

    WritePrivateProfileString(section, _T("hideCID"), account->hideCID, filename);

    WritePrivateProfileString(section, _T("voicemailNumber"), account->voicemailNumber, filename);

    WritePrivateProfileString(section, _T("transport"), account->transport, filename);
    WritePrivateProfileString(section, _T("publicAddr"), account->publicAddr, filename);
    WritePrivateProfileString(section, _T("SRTP"), account->srtp, filename);
    str.Format(_T("%d"), account->registerRefresh);
    WritePrivateProfileString(section, _T("registerRefresh"), str, filename);
    str.Format(_T("%d"), account->keepAlive);
    WritePrivateProfileString(section, _T("keepAlive"), str, filename);
    WritePrivateProfileString(section, _T("publish"), account->publish ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("ICE"), account->ice ? _T("1") : _T("0"), filename);

    WritePrivateProfileString(section, _T("allowRewrite"), account->allowRewrite ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("disableSessionTimer"), account->disableSessionTimer ? _T("1") : _T("0"), filename);
}

void AccountSettings::SettingsLoad(CString filename)
{
    if (filename.IsEmpty()) {
        filename = iniFile;
    }

    CString str;
    LPTSTR ptr;

    CString section = _T("Settings");

    // load user settings

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("singleMode"), _T("1"), ptr, 256, filename);
    str.ReleaseBuffer();
    singleMode = _wtoi(str);

    ptr = ringtone.GetBuffer(256);
    GetPrivateProfileString(section, _T("ringingSound"), NULL, ptr, 256, filename);
    ringtone.ReleaseBuffer();

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("volumeRing"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    volumeRing = str.IsEmpty() ? 100 : _wtoi(str);

    ptr = audioRingDevice.GetBuffer(256);
    GetPrivateProfileString(section, _T("audioRingDevice"), NULL, ptr, 256, filename);
    audioRingDevice.ReleaseBuffer();
    ptr = audioOutputDevice.GetBuffer(256);
    GetPrivateProfileString(section, _T("audioOutputDevice"), NULL, ptr, 256, filename);
    audioOutputDevice.ReleaseBuffer();
    ptr = audioInputDevice.GetBuffer(256);
    GetPrivateProfileString(section, _T("audioInputDevice"), NULL, ptr, 256, filename);
    audioInputDevice.ReleaseBuffer();


    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("micAmplification"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    micAmplification = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("swLevelAdjustment"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    swLevelAdjustment = _wtoi(str);

    ptr = audioCodecs.GetBuffer(512);
    GetPrivateProfileString(section, _T("audioCodecs"), _T(_GLOBAL_CODECS_ENABLED), ptr, 512, filename);
    audioCodecs.ReleaseBuffer();

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("VAD"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    vad = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("EC"), _T("1"), ptr, 256, filename);
    str.ReleaseBuffer();
    ec = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("forceCodec"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    forceCodec = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("opusStereo"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    opusStereo = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("disableMessaging"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    disableMessaging = _wtoi(str);

#ifdef _GLOBAL_VIDEO
    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("disableVideo"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    disableVideo = str == "1" ? 1 : 0;

    ptr = videoCaptureDevice.GetBuffer(256);
    GetPrivateProfileString(section, _T("videoCaptureDevice"), NULL, ptr, 256, filename);
    videoCaptureDevice.ReleaseBuffer();

    ptr = videoCodec.GetBuffer(256);
    GetPrivateProfileString(section, _T("videoCodec"), NULL, ptr, 256, filename);
    videoCodec.ReleaseBuffer();

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("videoH264"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    videoH264 = str == "0" ? 0 : 1;
    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("videoH263"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    videoH263 = str == "0" ? 0 : 1;
    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("videoVP8"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    videoVP8 = str == "0" ? 0 : 1;
    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("videoVP9"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    videoVP9 = str == "0" ? 0 : 1;

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("videoBitrate"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    videoBitrate = _wtoi(str);
#endif

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("rport"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    rport = str == "0" ? 0 : 1;

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("sourcePort"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    sourcePort = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("rtpPortMin"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    rtpPortMin = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("rtpPortMax"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    rtpPortMax = _wtoi(str);

    ptr = dnsSrvNs.GetBuffer(256);
    GetPrivateProfileString(section, _T("dnsSrvNs"), NULL, ptr, 256, filename);
    dnsSrvNs.ReleaseBuffer();
    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("dnsSrv"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    dnsSrv = str == "1" ? 1 : 0;

    ptr = stun.GetBuffer(256);
    GetPrivateProfileString(section, _T("STUN"), NULL, ptr, 256, filename);
    stun.ReleaseBuffer();
    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("enableSTUN"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    enableSTUN = str == "1" ? 1 : 0;

    CString iniFileRec = iniFile;

    ptr = recordingPath.GetBuffer(256);
    if (isPortable) {
        str = _T("Recordings");
    }
    else {
        LPTSTR ptr1 = str.GetBuffer(MAX_PATH);
        SHGetSpecialFolderPath(
            0,
            ptr1,
            CSIDL_DESKTOPDIRECTORY,
            FALSE);
        str.ReleaseBuffer();
        str.Append(_T("\\Recordings"));
    }
    GetPrivateProfileString(section, _T("recordingPath"), str, ptr, 256, iniFileRec);
    recordingPath.ReleaseBuffer();

    ptr = recordingFormat.GetBuffer(256);
    GetPrivateProfileString(section, _T("recordingFormat"), NULL, ptr, 256, iniFileRec);
    recordingFormat.ReleaseBuffer();

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("autoRecording"), NULL, ptr, 256, iniFileRec);
    str.ReleaseBuffer();
    autoRecording = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("recordingButton"), _T("1"), ptr, 256, iniFileRec);
    str.ReleaseBuffer();
    recordingButton = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("buttonAC"), _T("1"), ptr, 256, iniFileRec);
    str.ReleaseBuffer();
    buttonAC = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("buttonCONF"), _T("1"), ptr, 256, iniFileRec);
    str.ReleaseBuffer();
    buttonCONF = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("DTMFMethod"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    DTMFMethod = _wtoi(str);

    ptr = autoAnswer.GetBuffer(256);
    GetPrivateProfileString(section, _T("autoAnswer"), _T(_GLOBAL_SETT_AA_DEFAULT), ptr, 256, filename);
    autoAnswer.ReleaseBuffer();

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("autoAnswerDelay"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    autoAnswerDelay = _wtoi(str);

    ptr = autoAnswerNumber.GetBuffer(256);
    GetPrivateProfileString(section, _T("autoAnswerNumber"), NULL, ptr, 256, filename);
    autoAnswerNumber.ReleaseBuffer();

    ptr = autoAnswerCalls.GetBuffer(256);
    GetPrivateProfileString(section, _T("autoAnswerCalls"), NULL, ptr, 256, filename);
    autoAnswerCalls.ReleaseBuffer();

    ptr = forwarding.GetBuffer(256);
    GetPrivateProfileString(section, _T("forwarding"), NULL, ptr, 256, filename);
    forwarding.ReleaseBuffer();

    ptr = forwardingNumber.GetBuffer(256);
    GetPrivateProfileString(section, _T("forwardingNumber"), NULL, ptr, 256, filename);
    forwardingNumber.ReleaseBuffer();

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("forwardingDelay"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    forwardingDelay = _wtoi(str);

    ptr = featureCodeCP.GetBuffer(256);
    GetPrivateProfileString(section, _T("featureCodeCP"), _T("**"), ptr, 256, filename);
    featureCodeCP.ReleaseBuffer();

    ptr = featureCodeBT.GetBuffer(256);
    GetPrivateProfileString(section, _T("featureCodeBT"), _T("##"), ptr, 256, filename);
    featureCodeBT.ReleaseBuffer();

    ptr = featureCodeAT.GetBuffer(256);
    GetPrivateProfileString(section, _T("featureCodeAT"), _T("*2"), ptr, 256, filename);
    featureCodeAT.ReleaseBuffer();

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("enableFeatureCodeCP"), _T("1"), ptr, 256, iniFileRec);
    str.ReleaseBuffer();
    enableFeatureCodeCP = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("enableFeatureCodeBT"), _T("0"), ptr, 256, iniFileRec);
    str.ReleaseBuffer();
    enableFeatureCodeBT = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("enableFeatureCodeAT"), _T("0"), ptr, 256, iniFileRec);
    str.ReleaseBuffer();
    enableFeatureCodeAT = _wtoi(str);

    ptr = denyIncoming.GetBuffer(256);
    GetPrivateProfileString(section, _T("denyIncoming"), _T(_GLOBAL_SETT_DENYINC_DEFAULT), ptr, 256, filename);
    denyIncoming.ReleaseBuffer();

    //--
    ptr = usersDirectory.GetBuffer(256);
    GetPrivateProfileString(section, _T("usersDirectory"), NULL, ptr, 256, filename);
    usersDirectory.ReleaseBuffer();

    ptr = defaultAction.GetBuffer(256);
    GetPrivateProfileString(section, _T("defaultAction"), NULL, ptr, 256, filename);
    defaultAction.ReleaseBuffer();

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("enableMediaButtons"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    enableMediaButtons = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("headsetSupport"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    headsetSupport = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("localDTMF"), _T("1"), ptr, 256, filename);
    str.ReleaseBuffer();
    localDTMF = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("enableLog"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    enableLog = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("bringToFrontOnIncoming"), _T("1"), ptr, 256, filename);
    str.ReleaseBuffer();
    bringToFrontOnIncoming = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("enableLocalAccount"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    enableLocalAccount = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("randomAnswerBox"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    randomAnswerBox = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("disableNameLookup"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    disableNameLookup = _wtoi(str);

    crashReport = 0;

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("callWaiting"), _T("1"), ptr, 256, filename);
    str.ReleaseBuffer();
    callWaiting = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("multiMonitor"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    multiMonitor = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("networkChanges"), _T("1"), ptr, 256, filename);
    str.ReleaseBuffer();
    networkChanges = _wtoi(str);

    ptr = updatesInterval.GetBuffer(256);
    GetPrivateProfileString(section, _T("updatesInterval"), NULL, ptr, 256, filename);
    updatesInterval.ReleaseBuffer();

    // load ini settings

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("noResize"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    noResize = str == _T("1") ? 1 : 0;

    ptr = userAgent.GetBuffer(256);
    GetPrivateProfileString(section, _T("userAgent"), NULL, ptr, 256, filename);
    userAgent.ReleaseBuffer();

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("autoHangUpTime"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    autoHangUpTime = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("maxConcurrentCalls"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    maxConcurrentCalls = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("noIgnoreCall"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    noIgnoreCall = str == "1" ? 1 : 0;

    ptr = cmdOutgoingCall.GetBuffer(256);
    GetPrivateProfileString(section, _T("cmdOutgoingCall"), NULL, ptr, 256, filename);
    cmdOutgoingCall.ReleaseBuffer();

    ptr = cmdIncomingCall.GetBuffer(256);
    GetPrivateProfileString(section, _T("cmdIncomingCall"), NULL, ptr, 256, filename);
    cmdIncomingCall.ReleaseBuffer();

    ptr = cmdCallRing.GetBuffer(256);
    GetPrivateProfileString(section, _T("cmdCallRing"), NULL, ptr, 256, filename);
    cmdCallRing.ReleaseBuffer();

    ptr = cmdCallAnswer.GetBuffer(256);
    GetPrivateProfileString(section, _T("cmdCallAnswer"), NULL, ptr, 256, filename);
    cmdCallAnswer.ReleaseBuffer();

    ptr = cmdCallAnswerVideo.GetBuffer(256);
    GetPrivateProfileString(section, _T("cmdCallAnswerVideo"), NULL, ptr, 256, filename);
    cmdCallAnswerVideo.ReleaseBuffer();

    ptr = cmdCallBusy.GetBuffer(256);
    GetPrivateProfileString(section, _T("cmdCallBusy"), NULL, ptr, 256, filename);
    cmdCallBusy.ReleaseBuffer();

    ptr = cmdCallStart.GetBuffer(256);
    GetPrivateProfileString(section, _T("cmdCallStart"), NULL, ptr, 256, filename);
    cmdCallStart.ReleaseBuffer();

    ptr = cmdCallEnd.GetBuffer(256);
    GetPrivateProfileString(section, _T("cmdCallEnd"), NULL, ptr, 256, filename);
    cmdCallEnd.ReleaseBuffer();

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("minimized"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    minimized = _wtoi(str);

    ptr = portKnockerHost.GetBuffer(256);
    GetPrivateProfileString(section, _T("portKnockerHost"), NULL, ptr, 256, filename);
    portKnockerHost.ReleaseBuffer();

    ptr = portKnockerPorts.GetBuffer(256);
    GetPrivateProfileString(section, _T("portKnockerPorts"), NULL, ptr, 256, filename);
    portKnockerPorts.ReleaseBuffer();

    // load system settings

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("mainX"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    mainX = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("mainY"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    mainY = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("mainW"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    mainW = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("mainH"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    mainH = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("messagesX"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    messagesX = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("messagesY"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    messagesY = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("messagesW"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    messagesW = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("messagesH"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    messagesH = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("ringinX"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    ringinX = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("ringinY"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    ringinY = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("callsWidth0"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    callsWidth0 = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("callsWidth1"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    callsWidth1 = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("callsWidth2"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    callsWidth2 = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("callsWidth3"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    callsWidth3 = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("callsWidth4"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    callsWidth4 = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("callsWidth5"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    callsWidth5 = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("contactsWidth0"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    contactsWidth0 = _wtoi(str);
    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("contactsWidth1"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    contactsWidth1 = _wtoi(str);
    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("contactsWidth2"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    contactsWidth2 = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("volumeOutput"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    volumeOutput = str.IsEmpty() ? 100 : _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("volumeInput"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    volumeInput = str.IsEmpty() ? 100 : _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("activeTab"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    activeTab = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("FWD"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    FWD = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("AA"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    AA = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("AC"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    AC = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("DND"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    DND = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("alwaysOnTop"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    alwaysOnTop = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("enableShortcuts"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    enableShortcuts = _wtoi(str);

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("shortcutsBottom"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    shortcutsBottom = _wtoi(str);

    ptr = lastCallNumber.GetBuffer(256);
    GetPrivateProfileString(section, _T("lastCallNumber"), NULL, ptr, 256, filename);
    lastCallNumber.ReleaseBuffer();

    ptr = str.GetBuffer(256);
    GetPrivateProfileString(section, _T("lastCallHasVideo"), NULL, ptr, 256, filename);
    str.ReleaseBuffer();
    lastCallHasVideo = (str == _T("1"));
}

void AccountSettings::SettingsSave(CString filename)
{
    if (filename.IsEmpty()) {
        filename = iniFile;
    }

    CString str;
    LPTSTR ptr;

    //--
    CString section = _T("Settings");

    str.Format(_T("%d"), accountId);
    WritePrivateProfileString(section, _T("accountId"), str, filename);

    // save user settings

    WritePrivateProfileString(section, _T("singleMode"), singleMode ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("ringingSound"), ringtone, filename);
    str.Format(_T("%d"), volumeRing);
    WritePrivateProfileString(section, _T("volumeRing"), str, filename);
    WritePrivateProfileString(section, _T("audioRingDevice"), _T("\"") + audioRingDevice + _T("\""), filename);
    WritePrivateProfileString(section, _T("audioOutputDevice"), _T("\"") + audioOutputDevice + _T("\""), filename);
    WritePrivateProfileString(section, _T("audioInputDevice"), _T("\"") + audioInputDevice + _T("\""), filename);
    WritePrivateProfileString(section, _T("micAmplification"), micAmplification ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("swLevelAdjustment"), swLevelAdjustment ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("audioCodecs"), audioCodecs, filename);
    WritePrivateProfileString(section, _T("VAD"), vad ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("EC"), ec ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("forceCodec"), forceCodec ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("opusStereo"), opusStereo ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("disableMessaging"), disableMessaging ? _T("1") : _T("0"), filename);
#ifdef _GLOBAL_VIDEO
    WritePrivateProfileString(section, _T("disableVideo"), disableVideo ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("videoCaptureDevice"), _T("\"") + videoCaptureDevice + _T("\""), filename);
    WritePrivateProfileString(section, _T("videoCodec"), videoCodec, filename);
    WritePrivateProfileString(section, _T("videoH264"), videoH264 ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("videoH263"), videoH263 ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("videoVP8"), videoVP8 ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("videoVP9"), videoVP9 ? _T("1") : _T("0"), filename);
    str.Format(_T("%d"), videoBitrate);
    WritePrivateProfileString(section, _T("videoBitrate"), str, filename);
#endif
    WritePrivateProfileString(section, _T("rport"), rport ? _T("1") : _T("0"), filename);
    str.Format(_T("%d"), sourcePort);
    WritePrivateProfileString(section, _T("sourcePort"), str, filename);
    str.Format(_T("%d"), rtpPortMin);
    WritePrivateProfileString(section, _T("rtpPortMin"), str, filename);
    str.Format(_T("%d"), rtpPortMax);
    WritePrivateProfileString(section, _T("rtpPortMax"), str, filename);
    WritePrivateProfileString(section, _T("dnsSrvNs"), dnsSrvNs, filename);
    WritePrivateProfileString(section, _T("dnsSrv"), dnsSrv ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("STUN"), stun, filename);
    WritePrivateProfileString(section, _T("enableSTUN"), enableSTUN ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("recordingPath"), recordingPath, filename);
    WritePrivateProfileString(section, _T("recordingFormat"), recordingFormat, filename);
    WritePrivateProfileString(section, _T("autoRecording"), autoRecording ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("recordingButton"), recordingButton ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("buttonAC"), buttonAC ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("buttonCONF"), buttonCONF ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("DTMFMethod"), DTMFMethod == 1 ? _T("1") : (DTMFMethod == 2 ? _T("2") : (DTMFMethod == 3 ? _T("3") : _T("0"))), filename);
    WritePrivateProfileString(section, _T("autoAnswer"), autoAnswer, filename);
    str.Format(_T("%d"), autoAnswerDelay);
    WritePrivateProfileString(section, _T("autoAnswerDelay"), str, filename);
    WritePrivateProfileString(section, _T("autoAnswerNumber"), autoAnswerNumber, filename);
    WritePrivateProfileString(section, _T("autoAnswerCalls"), autoAnswerCalls, filename);
    WritePrivateProfileString(section, _T("forwarding"), forwarding, filename);
    WritePrivateProfileString(section, _T("forwardingNumber"), forwardingNumber, filename);
    str.Format(_T("%d"), forwardingDelay);
    WritePrivateProfileString(section, _T("forwardingDelay"), str, filename);
    WritePrivateProfileString(section, _T("featureCodeCP"), featureCodeCP, filename);
    WritePrivateProfileString(section, _T("featureCodeBT"), featureCodeBT, filename);
    WritePrivateProfileString(section, _T("featureCodeAT"), featureCodeAT, filename);
    WritePrivateProfileString(section, _T("enableFeatureCodeCP"), enableFeatureCodeCP ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("enableFeatureCodeBT"), enableFeatureCodeBT ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("enableFeatureCodeAT"), enableFeatureCodeAT ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("denyIncoming"), denyIncoming, filename);
    WritePrivateProfileString(section, _T("usersDirectory"), usersDirectory, filename);
    WritePrivateProfileString(section, _T("defaultAction"), defaultAction, filename);
    WritePrivateProfileString(section, _T("enableMediaButtons"), enableMediaButtons ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("headsetSupport"), headsetSupport ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("localDTMF"), localDTMF ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("enableLog"), enableLog ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("bringToFrontOnIncoming"), bringToFrontOnIncoming ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("enableLocalAccount"), enableLocalAccount ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("randomAnswerBox"), randomAnswerBox ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("disableNameLookup"), disableNameLookup ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("callWaiting"), callWaiting ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("multiMonitor"), multiMonitor ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("networkChanges"), networkChanges ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("updatesInterval"), updatesInterval, filename);

    // save ini settings

    str.Format(_T("%d"), noResize);
    WritePrivateProfileString(section, _T("noResize"), str, filename);

    WritePrivateProfileString(section, _T("userAgent"), userAgent, filename);

    str.Format(_T("%d"), autoHangUpTime);
    WritePrivateProfileString(section, _T("autoHangUpTime"), str, filename);

    str.Format(_T("%d"), maxConcurrentCalls);
    WritePrivateProfileString(section, _T("maxConcurrentCalls"), str, filename);
    WritePrivateProfileString(section, _T("noIgnoreCall"), noIgnoreCall ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("cmdOutgoingCall"), _T("\"") + cmdOutgoingCall + _T("\""), filename);
    WritePrivateProfileString(section, _T("cmdIncomingCall"), _T("\"") + cmdIncomingCall + _T("\""), filename);
    WritePrivateProfileString(section, _T("cmdCallRing"), _T("\"") + cmdCallRing + _T("\""), filename);
    WritePrivateProfileString(section, _T("cmdCallAnswer"), _T("\"") + cmdCallAnswer + _T("\""), filename);
    WritePrivateProfileString(section, _T("cmdCallAnswerVideo"), _T("\"") + cmdCallAnswerVideo + _T("\""), filename);
    WritePrivateProfileString(section, _T("cmdCallBusy"), _T("\"") + cmdCallBusy + _T("\""), filename);
    WritePrivateProfileString(section, _T("cmdCallStart"), _T("\"") + cmdCallStart + _T("\""), filename);
    WritePrivateProfileString(section, _T("cmdCallEnd"), _T("\"") + cmdCallEnd + _T("\""), filename);

    WritePrivateProfileString(section, _T("minimized"), minimized ? _T("1") : _T("0"), filename);

    WritePrivateProfileString(section, _T("portKnockerHost"), portKnockerHost, filename);

    WritePrivateProfileString(section, _T("portKnockerPorts"), portKnockerPorts, filename);

    // save system settings

    str.Format(_T("%d"), mainX);
    WritePrivateProfileString(section, _T("mainX"), str, filename);

    str.Format(_T("%d"), mainY);
    WritePrivateProfileString(section, _T("mainY"), str, filename);

    str.Format(_T("%d"), mainW);
    WritePrivateProfileString(section, _T("mainW"), str, filename);

    str.Format(_T("%d"), mainH);
    WritePrivateProfileString(section, _T("mainH"), str, filename);

    str.Format(_T("%d"), messagesX);
    WritePrivateProfileString(section, _T("messagesX"), str, filename);

    str.Format(_T("%d"), messagesY);
    WritePrivateProfileString(section, _T("messagesY"), str, filename);

    str.Format(_T("%d"), messagesW);
    WritePrivateProfileString(section, _T("messagesW"), str, filename);

    str.Format(_T("%d"), messagesH);
    WritePrivateProfileString(section, _T("messagesH"), str, filename);

    str.Format(_T("%d"), ringinX);
    WritePrivateProfileString(section, _T("ringinX"), str, filename);

    str.Format(_T("%d"), ringinY);
    WritePrivateProfileString(section, _T("ringinY"), str, filename);

    str.Format(_T("%d"), callsWidth0);
    WritePrivateProfileString(section, _T("callsWidth0"), str, filename);

    str.Format(_T("%d"), callsWidth1);
    WritePrivateProfileString(section, _T("callsWidth1"), str, filename);

    str.Format(_T("%d"), callsWidth2);
    WritePrivateProfileString(section, _T("callsWidth2"), str, filename);

    str.Format(_T("%d"), callsWidth3);
    WritePrivateProfileString(section, _T("callsWidth3"), str, filename);

    str.Format(_T("%d"), callsWidth4);
    WritePrivateProfileString(section, _T("callsWidth4"), str, filename);

    str.Format(_T("%d"), callsWidth5);
    WritePrivateProfileString(section, _T("callsWidth5"), str, filename);

    str.Format(_T("%d"), contactsWidth0);
    WritePrivateProfileString(section, _T("contactsWidth0"), str, filename);
    str.Format(_T("%d"), contactsWidth1);
    WritePrivateProfileString(section, _T("contactsWidth1"), str, filename);
    str.Format(_T("%d"), contactsWidth2);
    WritePrivateProfileString(section, _T("contactsWidth2"), str, filename);

    str.Format(_T("%d"), volumeOutput);
    WritePrivateProfileString(section, _T("volumeOutput"), str, filename);

    str.Format(_T("%d"), volumeInput);
    WritePrivateProfileString(section, _T("volumeInput"), str, filename);

    str.Format(_T("%d"), activeTab);
    WritePrivateProfileString(section, _T("activeTab"), str, filename);
    WritePrivateProfileString(section, _T("FWD"), FWD ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("AA"), AA ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("AC"), AC ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("DND"), DND ? _T("1") : _T("0"), filename);
    str.Format(_T("%d"), alwaysOnTop);
    WritePrivateProfileString(section, _T("alwaysOnTop"), str, filename);

    WritePrivateProfileString(section, _T("enableShortcuts"), enableShortcuts ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("shortcutsBottom"), shortcutsBottom ? _T("1") : _T("0"), filename);
    WritePrivateProfileString(section, _T("lastCallNumber"), lastCallNumber, filename);
    WritePrivateProfileString(section, _T("lastCallHasVideo"), lastCallHasVideo ? _T("1") : _T("0"), filename);
}

void AccountSettings::GlobalSave(CString filename)
{
    if (filename.IsEmpty()) {
        filename = iniFile;
    }
    CString str;
    CString section = _T("Global");

    str.Format(_T("%d"), checkUpdatesTime);
    WritePrivateProfileString(section, _T("checkUpdatesTime"), str, filename);
}

CString ShortcutEncode(Shortcut* pShortcut)
{
    CString data;
    data.Format(_T("%s;%s;%s;%s;%d;%s"), pShortcut->label, pShortcut->number, pShortcut->type, pShortcut->number2, pShortcut->presence, pShortcut->label2);
    return data;
}

void ShortcutDecode(const CString& str, Shortcut* pShortcut, bool noDefault)
{
    pShortcut->label.Empty();
    pShortcut->number.Empty();
    pShortcut->label2.Empty();
    pShortcut->number2.Empty();
    if (noDefault) {
        pShortcut->type.Empty();
    } else {
        pShortcut->type = MSIP_SHORTCUT_DTMF;
    }
    pShortcut->presence = false;

    CString rab;
    int begin;
    int end;
    begin = 0;
    end = str.Find(';', begin);
    if (end != -1) {
        pShortcut->label = str.Mid(begin, end - begin);
        begin = end + 1;
        end = str.Find(';', begin);
        if (end != -1) {
            pShortcut->number = str.Mid(begin, end - begin);
            begin = end + 1;
            end = str.Find(';', begin);
            if (end != -1) {
                pShortcut->type = str.Mid(begin, end - begin);
                begin = end + 1;
                end = str.Find(';', begin);
                if (end != -1) {
                    pShortcut->number2 = str.Mid(begin, end - begin);
                    begin = end + 1;
                    end = str.Find(';', begin);
                    if (end != -1) {
                        rab = str.Mid(begin, end - begin);
                        begin = end + 1;
                        end = str.Find(';', begin);
                        if (end != -1) {
                            pShortcut->label2 = str.Mid(begin, end - begin);
                        }
                        else {
                            pShortcut->label2 = str.Mid(begin);
                        }
                    }
                    else {
                        rab = str.Mid(begin);
                    }
                    pShortcut->presence = rab == _T("1");
                }
                else {
                    pShortcut->number2 = str.Mid(begin);
                }
            }
            else {
                pShortcut->type = str.Mid(begin);
            }
            if (pShortcut->type == _T("0")) {
                pShortcut->type = MSIP_SHORTCUT_CALL;
            }
            else if (pShortcut->type == _T("1")) {
                pShortcut->type = MSIP_SHORTCUT_VIDEOCALL;
            }
            else if (pShortcut->type == _T("2")) {
                pShortcut->type = MSIP_SHORTCUT_MESSAGE;
            }
            else if (pShortcut->type == _T("3")) {
                pShortcut->type = MSIP_SHORTCUT_DTMF;
            }
            else if (pShortcut->type == _T("4")) {
                pShortcut->type = MSIP_SHORTCUT_BT;
            }
        } else {
            pShortcut->number = str.Mid(begin);
        }
    }
}

void ShortcutsLoad(CString filename)
{
    if (filename.IsEmpty()) {
        filename = accountSettings.iniFile;
    }
    Shortcut shortcut;
    CString key;
    CString val;
    LPTSTR ptr = val.GetBuffer(1024);
    int i = 0;
    while (i < _GLOBAL_SHORTCUTS_QTY) {
        key.Format(_T("%d"), i);
        if (GetPrivateProfileString(_T("Shortcuts"), key, NULL, ptr, 1024, filename)) {
            ShortcutDecode(ptr, &shortcut);
            shortcuts.Add(shortcut);
        }
        else {
            break;
        }
        i++;
    }
    if (!shortcuts.GetCount()) {
    }
}

void ShortcutsSave(CString filename)
{
    if (filename.IsEmpty()) {
        filename = accountSettings.iniFile;
    }
    WritePrivateProfileSection(_T("Shortcuts"), NULL, filename);
    for (int i = 0; i < shortcuts.GetCount(); i++) {
        Shortcut* shortcut = &shortcuts.GetAt(i);
        CString key;
        key.Format(_T("%d"), i);
        WritePrivateProfileString(_T("Shortcuts"), key, ShortcutEncode(shortcut), filename);
    }
}

