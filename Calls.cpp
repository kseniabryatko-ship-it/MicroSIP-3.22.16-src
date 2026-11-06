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
#include "Calls.h"

#include "microsip.h"
#include "global.h"
#include "settings.h"
#include <pjsua-lib/pjsua.h>
#include <pjsua-lib/pjsua_internal.h>
#include "mainDlg.h"
#include "langpack.h"
#include "CSVFile.h"
#include "Markup.h"
#include "Domain/Call.h"
#include "Data/Database.h"
#include "Data/CallsRepository.h"
#include "Utils/StringConverter.h"

enum {
    MSIP_CALLS_COL_NAME,
    MSIP_CALLS_COL_NUMBER,
    MSIP_CALLS_COL_TIME,
    MSIP_CALLS_COL_DURATION,
    MSIP_CALLS_COL_INFO,
};

Calls::Calls(CWnd* pParent /*=NULL*/)
    : CBaseDialog(Calls::IDD, pParent)
{
    Create(IDD, pParent);
}

Calls::~Calls(void)
{
}

BOOL Calls::OnInitDialog()
{
    CBaseDialog::OnInitDialog();

    AutoMove(IDC_CALLS, 0, 0, 100, 100);
    AutoMove(IDC_SEARCH_PICTURE, 0, 100, 0, 0);
    AutoMove(IDC_FILER_VALUE, 0, 100, 100, 0);

    TranslateDialog(this->m_hWnd);

    m_nLastDay = 0;

    imageList = new CImageList();
    int b = MulDiv(16, dpiY, 96);
    imageList->Create(b, b, ILC_COLOR32, 3, 3);
    imageList->SetBkColor(RGB(255, 255, 255));
    imageList->Add(theApp.LoadIcon(IDI_CALL_OUT));
    imageList->Add(theApp.LoadIcon(IDI_CALL_IN));
    imageList->Add(theApp.LoadIcon(IDI_CALL_MISS));
    imageList->Add(theApp.LoadIcon(IDI_CALL_MISS_1));

    CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CALLS);
    //list->SetExtendedStyle( list->GetExtendedStyle() |  LVS_EX_FULLROWSELECT | LVS_EX_AUTOSIZECOLUMNS);
    list->SetExtendedStyle(list->GetExtendedStyle() | LVS_EX_FULLROWSELECT);
    list->SetImageList(imageList, LVSIL_SMALL);

    CFont* font = list->GetFont();
    LOGFONT lf;
    font->GetLogFont(&lf);
    lf.lfHeight = -MulDiv(12, dpiY, 96);
    font = new CFont();
    font->CreateFontIndirect(&lf);
    list->SetFont(font);
    ((CEdit*)GetDlgItem(IDC_FILER_VALUE))->SetFont(font);
    list->InsertColumn(MSIP_CALLS_COL_NAME, Translate(_T("Name")), LVCFMT_LEFT, accountSettings.callsWidth0 > 0 ? accountSettings.callsWidth0 : 160);
    list->InsertColumn(MSIP_CALLS_COL_NUMBER, Translate(_T("Number")), LVCFMT_LEFT, accountSettings.callsWidth1 > 0 ? accountSettings.callsWidth1 : 100);
    list->InsertColumn(MSIP_CALLS_COL_TIME, Translate(_T("Time")), LVCFMT_LEFT, accountSettings.callsWidth2 > 0 ? accountSettings.callsWidth2 : 135);
    list->InsertColumn(MSIP_CALLS_COL_DURATION, Translate(_T("Duration")), LVCFMT_LEFT, accountSettings.callsWidth3 > 0 ? accountSettings.callsWidth3 : 70);
    list->InsertColumn(MSIP_CALLS_COL_INFO, Translate(_T("Information")), LVCFMT_LEFT, accountSettings.callsWidth4 > 0 ? accountSettings.callsWidth4 : 120);

    CString filename = accountSettings.pathRoaming + _T("call_log.db");
    static Database db(CStringToUtf8(filename));
    db.InitCalls();
    static CallsService callsService(std::make_unique<CallsRepository>(db.Get()));

    m_callsService = &callsService;

    CallsLoad();

    SetTimer(IDT_TIMER_CALLS, 300 * 1000, NULL);

    return TRUE;
}

void Calls::OnCreated()
{
    m_list.SetSortColumn(2, false);
}

void Calls::PostNcDestroy()
{
    CBaseDialog::PostNcDestroy();
    //mainDlg->pageCalls = NULL;
    //delete imageList;
    //delete this;
}

void Calls::DoDataExchange(CDataExchange* pDX)
{
    CBaseDialog::DoDataExchange(pDX);
    DDX_Control(pDX, IDC_CALLS, m_list);
}

void Calls::OnTimer(UINT_PTR TimerVal)
{
    ReloadTime();
}


BEGIN_MESSAGE_MAP(Calls, CBaseDialog)
    ON_WM_CREATE()
    ON_WM_TIMER()
    ON_NOTIFY(HDN_ENDTRACK, 0, OnEndtrack)
    ON_BN_CLICKED(IDOK, OnBnClickedOk)
    ON_BN_CLICKED(IDCANCEL, OnBnClickedCancel)
    ON_EN_CHANGE(IDC_FILER_VALUE, OnFilterValueChange)
    ON_COMMAND(ID_CALL, OnMenuCall)
    ON_COMMAND(ID_CHAT, OnMenuChat)
    ON_COMMAND(ID_ADD, OnMenuAdd)
    ON_COMMAND(ID_COPY, OnMenuCopy)
    ON_COMMAND(ID_DELETE, OnMenuDelete)
    ON_COMMAND(ID_EXPORT, OnMenuExport)
    ON_COMMAND(ID_IMPORT, OnMenuImport)
    ON_NOTIFY(NM_DBLCLK, IDC_CALLS, &Calls::OnNMDblclkCalls)
    ON_MESSAGE(WM_CONTEXTMENU, OnContextMenu)
#ifdef _GLOBAL_VIDEO
    ON_COMMAND(ID_VIDEOCALL, OnMenuCallVideo)
#endif
END_MESSAGE_MAP()

BOOL Calls::PreTranslateMessage(MSG* pMsg)
{
    BOOL catched = FALSE;
    if (pMsg->message == WM_KEYDOWN) {
        if (pMsg->wParam == VK_ESCAPE) {
            CEdit* edit = (CEdit*)GetDlgItem(IDC_FILER_VALUE);
            if (edit == GetFocus()) {
                catched = TRUE;
                CString search;
                edit->GetWindowText(search);
                if (!search.IsEmpty()) {
                    FilterReset();
                }
            }
        }
        if (pMsg->wParam == VK_DELETE) {
            if (&m_list == GetFocus()) {
                catched = TRUE;
                OnMenuDelete();
            }
        }
    }
    if (!catched) {
        return CBaseDialog::PreTranslateMessage(pMsg);
    }
    else {
        return TRUE;
    }
}

void Calls::OnEndtrack(NMHDR* pNMHDR, LRESULT* pResult)
{
    HD_NOTIFY* phdn = (HD_NOTIFY*)pNMHDR;
    int width = phdn->pitem->cxy;
    switch (phdn->iItem) {
    case 0:
        accountSettings.callsWidth0 = width;
        break;
    case 1:
        accountSettings.callsWidth1 = width;
        break;
    case 2:
        accountSettings.callsWidth2 = width;
        break;
    case 3:
        accountSettings.callsWidth3 = width;
        break;
    case 4:
        accountSettings.callsWidth4 = width;
        break;
    }
    mainDlg->AccountSettingsPendingSave();
    *pResult = 0;
}


void Calls::OnBnClickedOk()
{
    CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CALLS);
    POSITION pos = list->GetFirstSelectedItemPosition();
    if (pos) {
        DefaultItemAction(list->GetNextSelectedItem(pos));
    }
}

void Calls::DefaultItemAction(int i)
{
    if (accountSettings.defaultAction.IsEmpty()) {
        MessageDlgOpen(accountSettings.singleMode);
    }
    else {
        if (accountSettings.defaultAction == _T("call")) {
            OnMenuCall();
        }
#ifdef _GLOBAL_VIDEO
        else if (accountSettings.defaultAction == _T("video")) {
            OnMenuCallVideo();
        }
#endif
        else {
            OnMenuChat();
        }
    }
}

void Calls::OnBnClickedCancel()
{
    mainDlg->ShowWindow(SW_HIDE);
}

void Calls::OnFilterValueChange()
{
    CallsClear();
    CallsLoad();
}

bool Calls::IsFiltered(const Call& call) {
    CEdit* edit = (CEdit*)GetDlgItem(IDC_FILER_VALUE);
    CString search;
    edit->GetWindowText(search);
    return m_callsService->IsCallFiletered(call, search);
}

void Calls::FilterReset()
{
    CEdit* edit = (CEdit*)GetDlgItem(IDC_FILER_VALUE);
    edit->SetWindowText(_T(""));
}

LRESULT Calls::OnContextMenu(WPARAM wParam, LPARAM lParam)
{
    int x = GET_X_LPARAM(lParam);
    int y = GET_Y_LPARAM(lParam);
    POINT pt = { x, y };
    RECT rc;
    CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CALLS);
    POSITION pos = list->GetFirstSelectedItemPosition();
    int selectedItem = -1;
    int selectedItem2 = -1;
    if (pos) {
        selectedItem = list->GetNextSelectedItem(pos);
    }
    if (selectedItem != -1) {
        selectedItem2 = list->GetNextSelectedItem(pos);
    }
    if (x != -1 || y != -1) {
        ScreenToClient(&pt);
        GetClientRect(&rc);
        if (!PtInRect(&rc, pt)) {
            x = y = -1;
        }
    }
    else {
        if (selectedItem != -1) {
            list->GetItemPosition(selectedItem, &pt);
            list->ClientToScreen(&pt);
            x = 40 + pt.x;
            y = 8 + pt.y;
        }
        else {
            ::ClientToScreen((HWND)wParam, &pt);
            x = 10 + pt.x;
            y = 26 + pt.y;
        }
    }
    if (x != -1 || y != -1) {
        CMenu menu;
        if (menu.LoadMenu(IDR_MENU_CONTACT)) {
            CMenu* tracker = menu.GetSubMenu(0);
            TranslateMenu(tracker->m_hMenu);
            bool disabled = selectedItem == -1;
            if (!disabled) {
                POSITION pos = list->GetFirstSelectedItemPosition();
                int i = list->GetNextSelectedItem(pos);
                Call* pCall = reinterpret_cast<Call*>(list->GetItemData(i));
                if (mainDlg->pageContacts->FindContact(pCall->number)) {
                    disabled = true;
                }
            }
            tracker->ModifyMenu(ID_ADD, MF_BYCOMMAND | (disabled ? MF_DISABLED : 0), ID_ADD, Translate(_T("Add Contact")));
            tracker->RemoveMenu(ID_EDIT, 0);
            if (selectedItem != -1) {
                if (selectedItem2 == -1) {
                    tracker->EnableMenuItem(ID_CALL, FALSE);
#ifdef _GLOBAL_VIDEO
                    tracker->EnableMenuItem(ID_VIDEOCALL, FALSE);
#endif
                    tracker->EnableMenuItem(ID_CHAT, FALSE);
                    tracker->EnableMenuItem(ID_COPY, FALSE);
                }
                else {
                    tracker->EnableMenuItem(ID_CALL, TRUE);
#ifdef _GLOBAL_VIDEO
                    tracker->EnableMenuItem(ID_VIDEOCALL, TRUE);
#endif
                    tracker->EnableMenuItem(ID_CHAT, TRUE);
                    tracker->EnableMenuItem(ID_COPY, TRUE);
                }
                tracker->EnableMenuItem(ID_DELETE, FALSE);
            }
            else {
                tracker->EnableMenuItem(ID_CALL, TRUE);
#ifdef _GLOBAL_VIDEO
                tracker->EnableMenuItem(ID_VIDEOCALL, TRUE);
#endif
                tracker->EnableMenuItem(ID_CHAT, TRUE);
                tracker->EnableMenuItem(ID_COPY, TRUE);
                tracker->EnableMenuItem(ID_DELETE, TRUE);
            }
            tracker->AppendMenu(0, MF_SEPARATOR);
            tracker->AppendMenu(MF_STRING, ID_EXPORT, Translate(_T("Export")));
            tracker->AppendMenu(MF_STRING, ID_IMPORT, Translate(_T("Import")));
#ifdef _GLOBAL_VIDEO
            if (accountSettings.disableVideo) {
                tracker->RemoveMenu(ID_VIDEOCALL, MF_BYCOMMAND);
            }
#endif
            if (accountSettings.disableMessaging) {
                tracker->RemoveMenu(ID_CHAT, MF_BYCOMMAND);
            }
            if (tracker->GetMenuItemCount() == 3) {
                tracker->RemoveMenu(0, MF_BYPOSITION);
            }
            tracker->TrackPopupMenu(0, x, y, this);
        }
        return TRUE;
    }
    return DefWindowProc(WM_CONTEXTMENU, wParam, lParam);
}

void Calls::MessageDlgOpen(BOOL isCall, BOOL hasVideo)
{
    if (accountSettings.singleMode && mainDlg->messagesDlg->GetCallsCount() && isCall) {
        mainDlg->GotoTab(0);
        return;
    }
    CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CALLS);
    POSITION pos = list->GetFirstSelectedItemPosition();
    if (pos) {
        int i = list->GetNextSelectedItem(pos);
        Call* pCall = reinterpret_cast<Call*>(list->GetItemData(i));
        CString number = pCall->number;
        if (isCall) {
            mainDlg->MakeCall(number, hasVideo, false, pCall->type != MSIP_CALL_TYPE_OUTGOING, pCall->name);
        }
        else {
            mainDlg->MessagesOpen(number, false, pCall->type != MSIP_CALL_TYPE_OUTGOING, pCall->name);
        }
    }
}

void Calls::OnNMDblclkCalls(NMHDR* pNMHDR, LRESULT* pResult)
{
    LPNMITEMACTIVATE pNMItemActivate = reinterpret_cast<LPNMITEMACTIVATE>(pNMHDR);
    if (pNMItemActivate->iItem != -1) {
        DefaultItemAction(pNMItemActivate->iItem);
    }
    *pResult = 0;
}

void Calls::OnMenuCall()
{
    MessageDlgOpen(TRUE);
}

#ifdef _GLOBAL_VIDEO
void Calls::OnMenuCallVideo()
{
    MessageDlgOpen(TRUE, TRUE);
}
#endif

void Calls::OnMenuChat()
{
    if (!accountSettings.disableMessaging) {
        MessageDlgOpen();
    }
}

void Calls::OnMenuAdd()
{
    mainDlg->pageContacts->OnMenuAdd();
    POSITION pos = m_list.GetFirstSelectedItemPosition();
    int nItem = m_list.GetNextSelectedItem(pos);
    Call* pCall = reinterpret_cast<Call*>(m_list.GetItemData(nItem));
    Contact contact;
    contact.number = pCall->number;
    contact.name = pCall->name;
    mainDlg->pageContacts->addDlg->Load(&contact);
}

void Calls::OnMenuCopy()
{
    CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CALLS);
    POSITION pos = list->GetFirstSelectedItemPosition();
    if (pos) {
        int i = list->GetNextSelectedItem(pos);
        Call* pCall = reinterpret_cast<Call*>(list->GetItemData(i));
        mainDlg->CopyStringToClipboard(pCall->number);
    }
}

void Calls::OnMenuDelete()
{
    POSITION pos = m_list.GetFirstSelectedItemPosition();
    if (!pos) {
        return;
    }
    if (MessageBox(Translate(_T("Are you sure you want to delete the selected items?")), Translate(_T("Delete Call Log")), MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) != IDYES) {
        return;
    }
    if (m_list.GetSelectedCount() < 100) {
        for (int i = m_list.GetItemCount() - 1; i >= 0; --i) {
            if (m_list.GetItemState(i, LVIS_SELECTED) & LVIS_SELECTED) {
                Call* pCall = reinterpret_cast<Call*>(m_list.GetItemData(i));
                m_callsService->DeleteCall(pCall->id);
                m_list.DeleteItem(i);
                delete pCall;

            }
        }
    }
    else {
        std::vector<int> ids;
        for (int i = 0; i < m_list.GetItemCount(); i++) {
            if (m_list.GetItemState(i, LVIS_SELECTED) & LVIS_SELECTED) {
                Call* pCall = reinterpret_cast<Call*>(m_list.GetItemData(i));
                ids.push_back(pCall->id);
            }
        }
        m_callsService->DeleteCalls(ids);
        OnFilterValueChange();
    }
}

void Calls::OnMenuExport()
{
    Export(true);
}

void Calls::Export(bool bFilter)
{
    CString defaultFilename;
    defaultFilename.Format(_T("%s Call Log"), _T(_GLOBAL_NAME_NICE));
    CString search;
    if (bFilter) {
        CEdit* edit = (CEdit*)GetDlgItem(IDC_FILER_VALUE);
        edit->GetWindowText(search);
    }
    if (!search.IsEmpty()) {
        defaultFilename.AppendFormat(_T(" %s"), search);
    }
    TCHAR szFilters[] = _T("CSV Files (*.csv)|*.csv||");
    CFileDialog dlgFile(FALSE, _T("csv"), MSIP::MakeFilenameFromString(defaultFilename), OFN_PATHMUSTEXIST | OFN_OVERWRITEPROMPT | OFN_HIDEREADONLY, szFilters, this);
    if (dlgFile.DoModal() == IDOK) {
        CString filename = dlgFile.GetPathName();
        if (dlgFile.GetFileExt().IsEmpty()) {
            filename.Append(_T(".csv"));
        }
        m_callsService->ExportCallsCSV(filename, search);
    }
}

void Calls::OnMenuImport()
{
    CFileDialog dlgFile(TRUE, _T("cvs"), 0, OFN_FILEMUSTEXIST | OFN_HIDEREADONLY, _T("CSV Files (*.csv)|*.csv|"), this);
    if (dlgFile.DoModal() == IDOK) {
        std::string error = m_callsService->ImportCallsCSV(dlgFile.GetPathName());
        if (error.empty()) {
            OnFilterValueChange();
        }
        else {
            AfxMessageBox(Utf8ToCString(error));
        }
    }
}

int Calls::FindListItem(int id)
{
    int count = m_list.GetItemCount();
    for (int i = 0; i < count; i++)
    {
        Call* pCall = reinterpret_cast<Call*>(m_list.GetItemData(i));
        if (pCall->id == id) {
            return i;
        }
    }
    return -1;
}

void Calls::Add(pj_str_t id, CString address, CString name, CString commands, int type, call_user_data* user_data)
{
    SIPURI sipuri;
    MSIP::ParseSIPURI(address, sipuri);

    CString numberLocal;
    CString numberLocalSuffix;
    if (sipuri.user.IsEmpty()) {
        numberLocal = sipuri.domain;
        numberLocalSuffix.Append(sipuri.suffix);
        numberLocalSuffix.Append(commands);
    }
    else {
        numberLocal = sipuri.user;
        if (!accountSettings.account.dialingPrefix.IsEmpty()) {
            if (numberLocal.Find(accountSettings.account.dialingPrefix) == 0) {
                numberLocal = numberLocal.Mid(accountSettings.account.dialingPrefix.GetLength());
            }
        }
        if (sipuri.suffix.IsEmpty() && (get_account_domain() == sipuri.domain || sipuri.domain.IsEmpty())) {
            numberLocalSuffix.Append(commands);
        }
        else {
            numberLocalSuffix.Append(_T("@"));
            numberLocalSuffix.Append(sipuri.domain);
            numberLocalSuffix.Append(sipuri.suffix);
            numberLocalSuffix.Append(commands);
        }
    }
    numberLocal.Append(numberLocalSuffix);

    CString callId = MSIP::PjToStr(&id);

    auto call = m_callsService->GetCallByCallId(callId);
    if (!call) {
        ReloadTime();
        call = std::make_unique<Call>();
        call->callId = callId;
        call->number = numberLocal;
        call->name = name;
        call->type = type;
        if (m_callsService->AddCall(*call)) {
            if (!IsFiltered(*call)) {
                AddListItem(std::move(call));
            }
        }
    }
    else {
        int nItem = FindListItem(call->id);
        bool changed = false;
        if (call->number != (numberLocal)) {
            call->number = (numberLocal);
            changed = true;
            if (nItem != -1) {
                m_list.SetItemText(nItem, MSIP_CALLS_COL_NUMBER, (call->number));
            }
        }
        if (call->name != (name)) {
            call->name = (name);
            changed = true;
            if (nItem != -1) {
                m_list.SetItemText(nItem, MSIP_CALLS_COL_NAME, (call->name));
            }
        }
        if (call->type != type) {
            call->type = type;
            changed = true;
            if (nItem != -1) {
                m_list.SetItem(nItem, 0, LVIF_IMAGE, NULL, type, 0, 0, 0);
            }
        }
        if (changed) {
            m_callsService->UpdateCall(*call);
            if (nItem != -1) {
                Call* pCall = reinterpret_cast<Call*>(m_list.GetItemData(nItem));
                delete pCall;
                m_list.SetItemData(nItem, reinterpret_cast<DWORD_PTR>(call.release()));
            }
        }
    }
}

void Calls::DeleteAll()
{
    m_callsService->DeleteAllCalls();
    CallsClear();
}

void Calls::SetName(pj_str_t id, CString name)
{
    CString callId = MSIP::PjToStr(&id);
    std::unique_ptr<Call> call = m_callsService->GetCallByCallId((callId));
    if (call) {
        call->name = (name);
        m_callsService->UpdateCall(*call);
        int nItem = FindListItem(call->id);
        if (nItem != -1) {
            Call* pCall = reinterpret_cast<Call*>(m_list.GetItemData(nItem));
            pCall->name = call->name;
            m_list.SetItemText(nItem, MSIP_CALLS_COL_NAME, name);
        }
    }
}

void Calls::SetDuration(pj_str_t id, int sec, int total) {
    CString callId = MSIP::PjToStr(&id);
    std::unique_ptr<Call> call = m_callsService->GetCallByCallId((callId));
    if (call) {
        call->duration = sec;
        m_callsService->UpdateCall(*call);
        int nItem = FindListItem(call->id);
        if (nItem != -1) {
            Call* pCall = reinterpret_cast<Call*>(m_list.GetItemData(nItem));
            pCall->duration = call->duration;
            m_list.SetItemText(nItem, MSIP_CALLS_COL_DURATION, MSIP::GetDuration(call->duration));
        }
    }
}

void Calls::SetInfo(pj_str_t id, CString info) {
    CString callId = MSIP::PjToStr(&id);
    std::unique_ptr<Call> call = m_callsService->GetCallByCallId((callId));
    if (call) {
        call->info = (info);
        m_callsService->UpdateCall(*call);
        int nItem = FindListItem(call->id);
        if (nItem != -1) {
            Call* pCall = reinterpret_cast<Call*>(m_list.GetItemData(nItem));
            pCall->info = call->info;
            m_list.SetItemText(nItem, MSIP_CALLS_COL_INFO, info);
        }
    }
}

void Calls::AddListItem(std::unique_ptr<Call> call, int pos)
{
    Call* pCall = call.release();
    CString number = pCall->number;
    CString name = pCall->name;
    int i = m_list.InsertItem(LVIF_TEXT | LVIF_PARAM | LVIF_IMAGE, pos, name, 0, 0, pCall->type, reinterpret_cast<LPARAM>(pCall));
    m_list.SetItemText(i, MSIP_CALLS_COL_NUMBER, number);
    m_list.SetItemText(i, MSIP_CALLS_COL_TIME, FormatTime(pCall->time));
    m_list.SetItemText(i, MSIP_CALLS_COL_DURATION, MSIP::GetDuration(pCall->duration));
    m_list.SetItemText(i, MSIP_CALLS_COL_INFO, (pCall->info));
}

CString Calls::FormatTime(int time, CTime* pTimeNow)
{
    CTime timeNow;
    if (!pTimeNow) {
        timeNow = CTime::GetCurrentTime();
        pTimeNow = &timeNow;
    }
    if (!m_nLastDay) {
        m_nLastDay = pTimeNow->GetDay();
    }
    CTime timeCall(time);
    return timeCall.Format(
        pTimeNow->GetYear() == timeCall.GetYear() &&
        pTimeNow->GetMonth() == timeCall.GetMonth() &&
        pTimeNow->GetDay() == timeCall.GetDay()
        ? _T("%X") : _T("%c")
    );
}

void Calls::ReloadTime()
{
    CTime timeNow = CTime::GetCurrentTime();
    if (m_nLastDay && m_nLastDay != timeNow.GetDay()) {
        m_nLastDay = timeNow.GetDay();
        int count = m_list.GetItemCount();
        for (int i = 0; i < count; i++) {
            Call* pCall = reinterpret_cast<Call*>(m_list.GetItemData(i));
            CTime timeCall(pCall->time);
            m_list.SetItemText(i, MSIP_CALLS_COL_TIME, FormatTime(pCall->time, &timeNow));
        }
    }
}

void Calls::CallsClear()
{
    int count = m_list.GetItemCount();
    for (int i = 0; i < count; i++) {
        Call* pCall = reinterpret_cast<Call*>(m_list.GetItemData(i));
        delete pCall;
    }
    m_list.DeleteAllItems();
}

void Calls::CallsLoad()
{
    CEdit* edit = (CEdit*)GetDlgItem(IDC_FILER_VALUE);
    CString search;
    edit->GetWindowText(search);
    auto calls = m_callsService->GetAllCalls();
    for (auto& call : calls) {
        if (!m_callsService->IsCallFiletered(*call, search)) {
            AddListItem(std::move(call));
        }
    }
    m_list.SortColumn(m_list.GetSortColumn(), m_list.IsAscending());
}

